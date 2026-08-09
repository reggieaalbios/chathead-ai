import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process'
import { createInterface } from 'node:readline'
import { join } from 'node:path'
import { app } from 'electron'
import { z } from 'zod'
import { PROTOCOL_VERSION, type BackendSnapshot, type OverlayPosition, type ProviderId, type ResolvedAppearance } from '../shared/backend'

const ipcErrorSchema = z.object({ code: z.string(), message: z.string(), recoverable: z.boolean() })
const messageSchema = z.object({ protocolVersion: z.number() }).passthrough()
const restartDelays = [500, 1_000, 2_000] as const

interface Pending { resolve(value: BackendSnapshot): void; reject(reason: Error): void; timer: NodeJS.Timeout }

export class SidecarManager {
  private child: ChildProcessWithoutNullStreams | undefined
  private pending = new Map<string, Pending>()
  private listeners = new Set<(snapshot: BackendSnapshot) => void>()
  private sequence = 0
  private restartCount = 0
  private intentionallyStopping = false
  private readyPromise: Promise<void> | undefined
  private resolveReady: (() => void) | undefined
  private rejectReady: ((reason: Error) => void) | undefined
  private overlayTheme: ResolvedAppearance = 'light'
  private overlayPosition: OverlayPosition = 'left'

  constructor(
    private readonly onUnavailable: (message: string) => void,
    private readonly onOpenExternal: (purpose: 'codexLogin', url: string) => void
  ) {}

  async start(): Promise<void> {
    if (this.child && !this.child.killed) return this.readyPromise
    this.intentionallyStopping = false
    this.readyPromise = new Promise<void>((resolve, reject) => { this.resolveReady = resolve; this.rejectReady = reject })
    const executable = process.env.CHATHEAD_SIDECAR_PATH ?? (app.isPackaged
      ? join(process.resourcesPath, 'sidecar', 'chathead-linux')
      : join(app.getAppPath(), 'target', 'debug', 'chathead-linux'))
    const nativeLibraryPath = app.isPackaged
      ? join(process.resourcesPath, 'native', 'lib')
      : join(app.getAppPath(), '.local-native', 'lib')
    const libraryPath = [nativeLibraryPath, process.env.LD_LIBRARY_PATH].filter(Boolean).join(':')
    this.child = spawn(executable, [], { stdio: ['pipe', 'pipe', 'pipe'], env: { ...process.env, LD_LIBRARY_PATH: libraryPath } })
    this.child.once('error', (error) => {
      this.rejectReady?.(error)
      this.rejectReady = undefined
      this.onUnavailable(`Could not start the native sidecar: ${error.message}`)
    })
    this.child.stderr.on('data', (chunk: Buffer) => process.stderr.write(`[chathead-linux] ${chunk.toString()}`))
    createInterface({ input: this.child.stdout }).on('line', (line) => this.handleLine(line))
    this.child.once('exit', (_code, signal) => this.handleExit(signal))
    try {
      await Promise.race([
        this.readyPromise,
        new Promise<never>((_, reject) => setTimeout(() => reject(new Error('sidecar handshake timed out')), 5_000))
      ])
      await this.request('setOverlayTheme', { theme: this.overlayTheme })
      await this.request('setOverlayPosition', { position: this.overlayPosition })
    } catch (error) {
      this.child?.kill('SIGTERM')
      throw error
    }
  }

  onSnapshotChanged(callback: (snapshot: BackendSnapshot) => void): () => void {
    this.listeners.add(callback)
    return () => this.listeners.delete(callback)
  }

  getSnapshot = (): Promise<BackendSnapshot> => this.request('getSnapshot')
  saveApiKey = (providerId: ProviderId, apiKey: string): Promise<BackendSnapshot> => this.request('saveApiKey', { providerId, apiKey })
  connectSubscription = (providerId: ProviderId): Promise<BackendSnapshot> => this.request('connectSubscription', { providerId })
  disconnectProvider = (providerId: ProviderId): Promise<BackendSnapshot> => this.request('disconnectProvider', { providerId })
  launchOverlay = (): Promise<BackendSnapshot> => this.request('launchOverlay')
  stopOverlay = (): Promise<BackendSnapshot> => this.request('stopOverlay')
  setOverlayTheme = (theme: ResolvedAppearance): Promise<BackendSnapshot> => {
    this.overlayTheme = theme
    return this.request('setOverlayTheme', { theme })
  }
  setOverlayPosition = (position: OverlayPosition): Promise<BackendSnapshot> => {
    this.overlayPosition = position
    return this.request('setOverlayPosition', { position })
  }

  async shutdown(): Promise<void> {
    this.intentionallyStopping = true
    const child = this.child
    if (!child) return
    try { await this.request('shutdown') } catch { /* process exit is also success here */ }
    if (child.exitCode === null) {
      await Promise.race([
        new Promise<void>((resolve) => child.once('exit', () => resolve())),
        new Promise<void>((resolve) => setTimeout(() => { child.kill('SIGTERM'); resolve() }, 2_000))
      ])
    }
    this.child = undefined
  }

  private request(method: string, params: Record<string, unknown> = {}): Promise<BackendSnapshot> {
    const child = this.child
    if (!child || child.killed || !child.stdin.writable) return Promise.reject(new Error('SIDECAR_UNAVAILABLE: native actions are temporarily disabled'))
    const id = `${process.pid}-${++this.sequence}`
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { this.pending.delete(id); reject(new Error(`SIDECAR_UNAVAILABLE: ${method} timed out`)) }, 30_000)
      this.pending.set(id, { resolve, reject, timer })
      child.stdin.write(`${JSON.stringify({ protocolVersion: PROTOCOL_VERSION, id, method, params })}\n`)
    })
  }

  private handleLine(line: string): void {
    let raw: unknown
    try { raw = JSON.parse(line) } catch { this.onUnavailable('The native sidecar returned malformed data.'); return }
    const parsed = messageSchema.safeParse(raw)
    if (!parsed.success || parsed.data.protocolVersion !== PROTOCOL_VERSION) {
      this.onUnavailable('Native sidecar protocol mismatch. Reinstall matching app components.')
      return
    }
    const message = parsed.data as Record<string, unknown>
    if (message.event === 'ready') {
      this.resolveReady?.()
      this.resolveReady = undefined
      this.rejectReady = undefined
      if (message.payload) this.emit(message.payload as BackendSnapshot)
      return
    }
    if (message.event === 'snapshotChanged' && message.payload) { this.emit(message.payload as BackendSnapshot); return }
    if (message.event === 'openExternal') {
      const payload = message.payload as { purpose?: unknown; url?: unknown } | undefined
      if (payload?.purpose === 'codexLogin' && typeof payload.url === 'string') this.onOpenExternal('codexLogin', payload.url)
      return
    }
    if (typeof message.id !== 'string') return
    const pending = this.pending.get(message.id)
    if (!pending) return
    clearTimeout(pending.timer)
    this.pending.delete(message.id)
    if (message.error) {
      const error = ipcErrorSchema.parse(message.error)
      pending.reject(Object.assign(new Error(error.message), error))
    } else {
      pending.resolve(message.result as BackendSnapshot)
    }
  }

  private emit(snapshot: BackendSnapshot): void { for (const listener of this.listeners) listener(snapshot) }

  private handleExit(signal: NodeJS.Signals | null): void {
    this.child = undefined
    for (const pending of this.pending.values()) { clearTimeout(pending.timer); pending.reject(new Error('SIDECAR_UNAVAILABLE: sidecar exited')) }
    this.pending.clear()
    if (this.intentionallyStopping) return
    const delay = restartDelays[this.restartCount]
    if (delay === undefined) { this.onUnavailable(`Native sidecar stopped${signal ? ` (${signal})` : ''}. Restart ChatHead to retry.`); return }
    this.restartCount += 1
    setTimeout(() => { void this.start().catch((error: Error) => this.onUnavailable(error.message)) }, delay)
  }
}
