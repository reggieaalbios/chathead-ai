import { contextBridge, ipcRenderer } from 'electron'
import type { BackendApi, BackendSnapshot, OverlayPosition, ProviderId, ResolvedAppearance } from '../shared/backend'

const backend: BackendApi = Object.freeze({
  getSnapshot: () => ipcRenderer.invoke('backend:getSnapshot'),
  saveApiKey: (providerId: ProviderId, apiKey: string) => ipcRenderer.invoke('backend:saveApiKey', providerId, apiKey),
  connectSubscription: (providerId: ProviderId) => ipcRenderer.invoke('backend:connectSubscription', providerId),
  disconnectProvider: (providerId: ProviderId) => ipcRenderer.invoke('backend:disconnectProvider', providerId),
  launchOverlay: () => ipcRenderer.invoke('backend:launchOverlay'),
  stopOverlay: () => ipcRenderer.invoke('backend:stopOverlay'),
  setOverlayTheme: (theme: ResolvedAppearance) => ipcRenderer.invoke('backend:setOverlayTheme', theme),
  setOverlayPosition: (position: OverlayPosition) => ipcRenderer.invoke('backend:setOverlayPosition', position),
  shutdown: () => ipcRenderer.invoke('backend:shutdown'),
  onSnapshotChanged: (callback: (snapshot: BackendSnapshot) => void) => {
    const listener = (_event: Electron.IpcRendererEvent, snapshot: BackendSnapshot): void => callback(snapshot)
    ipcRenderer.on('backend:snapshotChanged', listener)
    return () => ipcRenderer.removeListener('backend:snapshotChanged', listener)
  }
})

contextBridge.exposeInMainWorld('chathead', Object.freeze({
  backend,
  window: Object.freeze({ minimize: () => ipcRenderer.send('window:minimize'), close: () => ipcRenderer.send('window:close') }),
  onUnavailable: (callback: (message: string) => void) => {
    const listener = (_event: Electron.IpcRendererEvent, message: string): void => callback(message)
    ipcRenderer.on('backend:unavailable', listener)
    return () => ipcRenderer.removeListener('backend:unavailable', listener)
  }
}))
