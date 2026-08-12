import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process'
import { createInterface } from 'node:readline'
import { join } from 'node:path'
import { app } from 'electron'
import { z } from 'zod'
import { PROTOCOL_VERSION, backendSnapshotSchema, defaultPanelSize, panelSizeSchema, panelZoomSchema, type BackendSnapshot, type PanelPosition, type PanelSize, type PanelZoom, type ProviderId, type ResolvedAppearance, type VoiceInteractionMode, type VoiceModelId, type VoiceSubmissionMode } from '../shared/backend'

const ipcErrorSchema = z.object({ code: z.string(), message: z.string(), recoverable: z.boolean() })
const messageSchema = z.object({ protocolVersion: z.number() }).passthrough()
const restartDelays = [500, 1_000, 2_000] as const

interface Pending { resolve(value: BackendSnapshot): void; reject(reason: Error): void; timer: NodeJS.Timeout }

export class SidecarManager {
  private child: ChildProcessWithoutNullStreams | undefined
  private pending = new Map<string, Pending>()
  private listeners = new Set<(snapshot: BackendSnapshot) => void>()
  private voiceLevelListeners = new Set<(level: number) => void>()
  private panelZoomListeners = new Set<(zoom: PanelZoom) => void>()
  private panelSizeListeners = new Set<(size: PanelSize) => void>()
  private sequence = 0
  private restartCount = 0
  private intentionallyStopping = false
  private readyPromise: Promise<void> | undefined
  private resolveReady: (() => void) | undefined
  private rejectReady: ((reason: Error) => void) | undefined
  private overlayTheme: ResolvedAppearance = 'light'
  private panelPosition: PanelPosition = 'right'
  private panelZoom: PanelZoom = 100
  private panelSize: PanelSize = defaultPanelSize

  constructor(
    private readonly onUnavailable: (message: string) => void,
    private readonly onOpenExternal: (purpose: 'codexLogin', url: string) => void,
    private readonly onOpenSettings: () => void
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
      await this.request('setPanelPosition', { position: this.panelPosition })
      await this.request('setPanelZoom', { zoom: this.panelZoom })
      await this.request('setPanelSize', { size: this.panelSize })
    } catch (error) {
      this.child?.kill('SIGTERM')
      throw error
    }
  }

  onSnapshotChanged(callback: (snapshot: BackendSnapshot) => void): () => void {
    this.listeners.add(callback)
    return () => this.listeners.delete(callback)
  }

  onVoiceLevelChanged(callback: (level: number) => void): () => void {
    this.voiceLevelListeners.add(callback)
    return () => this.voiceLevelListeners.delete(callback)
  }

  onPanelZoomChanged(callback: (zoom: PanelZoom) => void): () => void {
    this.panelZoomListeners.add(callback)
    return () => this.panelZoomListeners.delete(callback)
  }

  onPanelSizeChanged(callback: (size: PanelSize) => void): () => void {
    this.panelSizeListeners.add(callback)
    return () => this.panelSizeListeners.delete(callback)
  }

  getSnapshot = (): Promise<BackendSnapshot> => this.request('getSnapshot')
  saveApiKey = (providerId: ProviderId, apiKey: string): Promise<BackendSnapshot> => this.request('saveApiKey', { providerId, apiKey })
  connectSubscription = (providerId: ProviderId): Promise<BackendSnapshot> => this.request('connectSubscription', { providerId })
  disconnectProvider = (providerId: ProviderId): Promise<BackendSnapshot> => this.request('disconnectProvider', { providerId })
  launchOverlay = (): Promise<BackendSnapshot> => this.request('launchOverlay')
  stopOverlay = (): Promise<BackendSnapshot> => this.request('stopOverlay')
  refreshDesktopIntegration = (): Promise<BackendSnapshot> => this.request('refreshDesktopIntegration')
  setOverlayTheme = (theme: ResolvedAppearance): Promise<BackendSnapshot> => {
    this.overlayTheme = theme
    return this.request('setOverlayTheme', { theme })
  }
  setPanelPosition = (position: PanelPosition): Promise<BackendSnapshot> => {
    this.panelPosition = position
    return this.request('setPanelPosition', { position })
  }
  setPanelZoom = (zoom: PanelZoom): Promise<BackendSnapshot> => {
    const parsedZoom = panelZoomSchema.parse(zoom)
    this.panelZoom = parsedZoom
    return this.request('setPanelZoom', { zoom: parsedZoom })
  }
  setPanelSize = (size: PanelSize): Promise<BackendSnapshot> => {
    const parsedSize = panelSizeSchema.parse(size)
    this.panelSize = parsedSize
    return this.request('setPanelSize', { size: parsedSize })
  }
  setVoiceEnabled = (enabled: boolean): Promise<BackendSnapshot> => this.request('setVoiceEnabled', { enabled })
  setVoiceInputDevice = (deviceId?: string): Promise<BackendSnapshot> => this.request('setVoiceInputDevice', { deviceId })
  setVoiceInteractionMode = (mode: VoiceInteractionMode): Promise<BackendSnapshot> => this.request('setVoiceInteractionMode', { mode })
  setVoiceSubmissionMode = (mode: VoiceSubmissionMode): Promise<BackendSnapshot> => this.request('setVoiceSubmissionMode', { mode })
  refreshVoiceDevices = (): Promise<BackendSnapshot> => this.request('refreshVoiceDevices')
  retryVoiceSetup = (): Promise<BackendSnapshot> => this.request('retryVoiceSetup')
  setVoiceModel = (modelId: VoiceModelId): Promise<BackendSnapshot> => this.request('setVoiceModel', { modelId })
  downloadVoiceModel = (modelId: VoiceModelId): Promise<BackendSnapshot> => this.request('downloadVoiceModel', { modelId })
  cancelVoiceModelDownload = (modelId: VoiceModelId): Promise<BackendSnapshot> => this.request('cancelVoiceModelDownload', { modelId })
  removeVoiceModel = (modelId: VoiceModelId): Promise<BackendSnapshot> => this.request('removeVoiceModel', { modelId })
  startVoiceTest = (): Promise<BackendSnapshot> => this.request('startVoiceTest')
  stopVoiceTest = (): Promise<BackendSnapshot> => this.request('stopVoiceTest')

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
      const snapshot = backendSnapshotSchema.safeParse(message.payload)
      if (!snapshot.success) {
        const error = new Error('PROTOCOL_MISMATCH: native sidecar returned an invalid ready snapshot')
        this.rejectReady?.(error)
        this.resolveReady = undefined
        this.rejectReady = undefined
        this.onUnavailable(error.message)
        return
      }
      this.emit(snapshot.data)
      this.resolveReady?.()
      this.resolveReady = undefined
      this.rejectReady = undefined
      return
    }
    if (message.event === 'snapshotChanged' && message.payload) {
      const snapshot = backendSnapshotSchema.safeParse(message.payload)
      if (snapshot.success) this.emit(snapshot.data)
      else this.onUnavailable('The native sidecar returned an invalid snapshot.')
      return
    }
    if (message.event === 'openExternal') {
      const payload = message.payload as { purpose?: unknown; url?: unknown } | undefined
      if (payload?.purpose === 'codexLogin' && typeof payload.url === 'string') this.onOpenExternal('codexLogin', payload.url)
      return
    }
    if (message.event === 'openSettings') { this.onOpenSettings(); return }
    if (message.event === 'voiceLevelChanged') {
      const payload = message.payload as { level?: unknown } | undefined
      if (typeof payload?.level === 'number') for (const listener of this.voiceLevelListeners) listener(payload.level)
      return
    }
    if (message.event === 'panelZoomChanged') {
      const parsedZoom = panelZoomSchema.safeParse(message.payload)
      if (parsedZoom.success && parsedZoom.data !== this.panelZoom) {
        this.panelZoom = parsedZoom.data
        for (const listener of this.panelZoomListeners) listener(parsedZoom.data)
      }
      return
    }
    if (message.event === 'panelSizeChanged') {
      const parsedSize = panelSizeSchema.safeParse(message.payload)
      if (parsedSize.success && (parsedSize.data.width !== this.panelSize.width || parsedSize.data.height !== this.panelSize.height)) {
        this.panelSize = parsedSize.data
        for (const listener of this.panelSizeListeners) listener(parsedSize.data)
      }
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
      const snapshot = backendSnapshotSchema.safeParse(message.result)
      if (snapshot.success) pending.resolve(snapshot.data)
      else pending.reject(new Error('PROTOCOL_MISMATCH: native sidecar returned an invalid result snapshot'))
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
