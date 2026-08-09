import { join } from 'node:path'
import { app, BrowserWindow, ipcMain, Menu, nativeImage, session, shell, Tray } from 'electron'
import type { BackendSnapshot, OverlayPosition, ProviderId, ResolvedAppearance } from '../shared/backend'
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
    { label: running ? 'Stop ChatHead' : 'Launch ChatHead', enabled: latestSnapshot?.launchReadiness === 'ready' || running, click: () => void (running ? sidecar.stopOverlay() : sidecar.launchOverlay()) },
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
  ipcMain.handle('backend:setOverlayTheme', (_event, theme: ResolvedAppearance) => sidecar.setOverlayTheme(theme))
  ipcMain.handle('backend:setOverlayPosition', (_event, position: OverlayPosition) => sidecar.setOverlayPosition(position))
  ipcMain.handle('backend:shutdown', () => sidecar.shutdown())
  ipcMain.on('window:minimize', () => window?.minimize())
  ipcMain.on('window:close', () => window?.hide())
}

function installContentSecurityPolicy(): void {
  const policy = app.isPackaged
    ? "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
    : `default-src 'self'; script-src 'self' 'unsafe-eval' 'nonce-${developmentCspNonce}'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self' ws://localhost:* http://localhost:*`
  session.defaultSession.webRequest.onHeadersReceived((details, callback) => callback({ responseHeaders: { ...details.responseHeaders, 'Content-Security-Policy': [policy] } }))
}
