import { join } from 'node:path'
import { execFile } from 'node:child_process'
import { mkdtemp, readdir, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { app, BrowserWindow, ipcMain, Menu, nativeImage, session, shell, Tray } from 'electron'
import type { BackendSnapshot, PanelPosition, PanelSize, PanelZoom, ProviderId, ResolvedAppearance, VoiceInteractionMode, VoiceModelId, VoiceSubmissionMode } from '../shared/backend'
import { SidecarManager } from './sidecar'
import { approvedCodexLoginUrl } from './external'

let window: BrowserWindow | undefined
let tray: Tray | undefined
let quitting = false
let latestSnapshot: BackendSnapshot | undefined
const developmentCspNonce = 'chathead-vite-development'

const sidecar = new SidecarManager(
  (message) => window?.webContents.send('backend:unavailable', message),
  (_purpose, url) => {
    const approved = approvedCodexLoginUrl(url)
    if (approved) void shell.openExternal(approved)
    else window?.webContents.send('backend:unavailable', 'Codex returned an unapproved authentication address.')
  },
  () => {
    showWindow()
    window?.webContents.send('window:showSettings')
  }
)
const gotLock = app.requestSingleInstanceLock()
if (!gotLock) app.quit()

app.on('second-instance', () => showWindow())
app.whenReady().then(async () => {
  installContentSecurityPolicy()
  createWindow()
  createTray()
  sidecar.onSnapshotChanged((snapshot) => {
    latestSnapshot = snapshot
    window?.webContents.send('backend:snapshotChanged', snapshot)
    refreshTrayMenu()
  })
  sidecar.onVoiceLevelChanged((level) => window?.webContents.send('backend:voiceLevelChanged', level))
  sidecar.onPanelZoomChanged((zoom) => window?.webContents.send('backend:panelZoomChanged', zoom))
  sidecar.onPanelSizeChanged((size) => window?.webContents.send('backend:panelSizeChanged', size))
  registerIpc()
  try { await sidecar.start() } catch (error) { window?.webContents.send('backend:unavailable', error instanceof Error ? error.message : String(error)) }
})

app.on('activate', showWindow)
app.on('before-quit', (event) => {
  if (quitting) return
  event.preventDefault()
  quitting = true
  void sidecar.shutdown().finally(() => app.quit())
})
app.on('window-all-closed', () => { /* tray process intentionally remains active */ })

function createWindow(): void {
  window = new BrowserWindow({
    width: 880, height: 600, minWidth: 880, maxWidth: 880, minHeight: 600, maxHeight: 600,
    // Keep the fixed dimensions aligned with the renderer's 880x600 shell.
    // Without this, the platform window decoration metrics are included in
    // the requested size and leave an empty strip on the right and bottom.
    useContentSize: true, frame: false, transparent: false, resizable: false, show: false, backgroundColor: '#eef6ff',
    webPreferences: {
      preload: join(__dirname, '../preload/index.cjs'),
      contextIsolation: true, nodeIntegration: false, sandbox: true, devTools: !app.isPackaged
    }
  })
  window.on('close', (event) => { if (!quitting) { event.preventDefault(); window?.hide() } })
  window.once('ready-to-show', () => window?.show())
  window.webContents.setWindowOpenHandler(() => ({ action: 'deny' }))
  window.webContents.on('will-navigate', (event) => event.preventDefault())
  if (!app.isPackaged && process.env.ELECTRON_RENDERER_URL) void window.loadURL(process.env.ELECTRON_RENDERER_URL)
  else void window.loadFile(join(__dirname, '../renderer/index.html'))
}

function createTray(): void {
  const iconPath = app.isPackaged
    ? join(process.resourcesPath, 'provider-icon.png')
    : join(app.getAppPath(), 'src/assets/provider/dark/image-Photoroom (7).png')
  tray = new Tray(nativeImage.createFromPath(iconPath).resize({ width: 20, height: 20 }))
  tray.setToolTip('ChatHead AI')
  tray.on('double-click', showWindow)
  refreshTrayMenu()
}

function refreshTrayMenu(): void {
  const running = latestSnapshot?.overlayRunning ?? false
  tray?.setContextMenu(Menu.buildFromTemplate([
    { label: 'Open Settings', click: showWindow },
    { label: running ? 'Stop ChatHead' : 'Launch ChatHead', enabled: latestSnapshot?.launchReadiness.ready === true || running, click: () => void (running ? sidecar.stopOverlay() : sidecar.launchOverlay()) },
    { type: 'separator' },
    { label: 'Quit', click: () => app.quit() }
  ]))
}

function showWindow(): void {
  if (!window) return
  window.show()
  if (window.isMinimized()) window.restore()
  window.focus()
}

function registerIpc(): void {
  ipcMain.handle('backend:getSnapshot', () => sidecar.getSnapshot())
  ipcMain.handle('backend:saveApiKey', (_event, providerId: ProviderId, apiKey: string) => sidecar.saveApiKey(providerId, apiKey))
  ipcMain.handle('backend:connectSubscription', (_event, providerId: ProviderId) => sidecar.connectSubscription(providerId))
  ipcMain.handle('backend:disconnectProvider', (_event, providerId: ProviderId) => sidecar.disconnectProvider(providerId))
  ipcMain.handle('backend:launchOverlay', () => sidecar.launchOverlay())
  ipcMain.handle('backend:stopOverlay', () => sidecar.stopOverlay())
  ipcMain.handle('backend:refreshDesktopIntegration', () => sidecar.refreshDesktopIntegration())
  ipcMain.handle('backend:installDesktopIntegration', async () => {
    await installBundledGnomeExtension()
    return sidecar.refreshDesktopIntegration()
  })
  ipcMain.handle('backend:setOverlayTheme', (_event, theme: ResolvedAppearance) => sidecar.setOverlayTheme(theme))
  ipcMain.handle('backend:setPanelPosition', (_event, position: PanelPosition) => sidecar.setPanelPosition(position))
  ipcMain.handle('backend:setPanelZoom', (_event, zoom: PanelZoom) => sidecar.setPanelZoom(zoom))
  ipcMain.handle('backend:setPanelSize', (_event, size: PanelSize) => sidecar.setPanelSize(size))
  ipcMain.handle('backend:setVoiceEnabled', (_event, enabled: boolean) => sidecar.setVoiceEnabled(enabled))
  ipcMain.handle('backend:setVoiceInputDevice', (_event, deviceId?: string) => sidecar.setVoiceInputDevice(deviceId))
  ipcMain.handle('backend:setVoiceInteractionMode', (_event, mode: VoiceInteractionMode) => sidecar.setVoiceInteractionMode(mode))
  ipcMain.handle('backend:setVoiceSubmissionMode', (_event, mode: VoiceSubmissionMode) => sidecar.setVoiceSubmissionMode(mode))
  ipcMain.handle('backend:refreshVoiceDevices', () => sidecar.refreshVoiceDevices())
  ipcMain.handle('backend:retryVoiceSetup', () => sidecar.retryVoiceSetup())
  ipcMain.handle('backend:setVoiceModel', (_event, modelId: VoiceModelId) => sidecar.setVoiceModel(modelId))
  ipcMain.handle('backend:downloadVoiceModel', (_event, modelId: VoiceModelId) => sidecar.downloadVoiceModel(modelId))
  ipcMain.handle('backend:cancelVoiceModelDownload', (_event, modelId: VoiceModelId) => sidecar.cancelVoiceModelDownload(modelId))
  ipcMain.handle('backend:removeVoiceModel', (_event, modelId: VoiceModelId) => sidecar.removeVoiceModel(modelId))
  ipcMain.handle('backend:startVoiceTest', () => sidecar.startVoiceTest())
  ipcMain.handle('backend:stopVoiceTest', () => sidecar.stopVoiceTest())
  ipcMain.handle('backend:shutdown', () => sidecar.shutdown())
  ipcMain.on('window:minimize', () => window?.minimize())
  ipcMain.on('window:close', () => window?.hide())
}

async function installBundledGnomeExtension(): Promise<void> {
  if (latestSnapshot?.desktopIntegration.kind !== 'gnomeShell') {
    throw new Error('DESKTOP_INTEGRATION_UNAVAILABLE: GNOME Shell integration is not applicable in this session.')
  }
  if (latestSnapshot.desktopIntegration.gnomeVersion !== '46') {
    throw new Error('DESKTOP_INTEGRATION_UNAVAILABLE: ChatHead currently supports GNOME Shell 46 only.')
  }

  const source = app.isPackaged
    ? join(process.resourcesPath, 'gnome-extension', 'chathead-ai@io.github.chathead-ai')
    : join(app.getAppPath(), 'gnome-extension', 'chathead-ai@io.github.chathead-ai')
  const staging = await mkdtemp(join(tmpdir(), 'chathead-gnome-extension-'))
  try {
    await runExtensionCommand([
      'pack', '--force', '--out-dir', staging,
      '--extra-source', 'chathead-orb.svg', '.'
    ], source)
    const archive = (await readdir(staging)).find((entry) => entry.endsWith('.zip'))
    if (!archive) throw new Error('GNOME extension packaging did not produce an archive.')
    await runExtensionCommand(['install', '--force', join(staging, archive)])
    try {
      await runExtensionCommand(['enable', 'chathead-ai@io.github.chathead-ai'])
    } catch (error) {
      // GNOME Shell 46 does not necessarily discover a newly installed local
      // extension until the next login. The files are installed successfully;
      // readiness will report the extension as disabled until Shell loads it.
      if (!(error instanceof ExtensionCommandError) || !error.extensionIsUnknown) throw error
    }
  } finally {
    await rm(staging, { recursive: true, force: true })
  }
}

class ExtensionCommandError extends Error {
  readonly extensionIsUnknown: boolean

  constructor(action: string, stderr: string) {
    const diagnostic = sanitizeCommandDiagnostic(stderr)
    super(`GNOME could not ${action} the ChatHead extension.${diagnostic ? ` ${diagnostic}` : ''}`)
    this.name = 'ExtensionCommandError'
    this.extensionIsUnknown = /(?:does not exist|doesn't exist)/i.test(stderr)
  }
}

function sanitizeCommandDiagnostic(stderr: string): string {
  const line = stderr.split(/\r?\n/u).map((value) => value.trim()).find(Boolean)
  if (!line) return ''
  return Array.from(line, (character) => {
    const codePoint = character.codePointAt(0) ?? 0
    return codePoint < 32 || codePoint === 127 ? '' : character
  }).join('').slice(0, 240)
}

function runExtensionCommand(arguments_: string[], cwd?: string): Promise<void> {
  return new Promise((resolve, reject) => {
    execFile('gnome-extensions', arguments_, { cwd, timeout: 30_000, windowsHide: true }, (error, _stdout, stderr) => {
      if (error) reject(new ExtensionCommandError(arguments_[0] ?? 'manage', stderr))
      else resolve()
    })
  })
}

function installContentSecurityPolicy(): void {
  const policy = app.isPackaged
    ? "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
    : `default-src 'self'; script-src 'self' 'unsafe-eval' 'nonce-${developmentCspNonce}'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self' ws://localhost:* http://localhost:*`
  session.defaultSession.webRequest.onHeadersReceived((details, callback) => callback({ responseHeaders: { ...details.responseHeaders, 'Content-Security-Policy': [policy] } }))
}
