import { contextBridge, ipcRenderer } from 'electron'
import type { BackendApi, BackendSnapshot, PanelPosition, PanelSize, PanelZoom, ProviderId, ResolvedAppearance, VoiceInteractionMode, VoiceModelId, VoiceSubmissionMode } from '../shared/backend'

const backend: BackendApi = Object.freeze({
  getSnapshot: () => ipcRenderer.invoke('backend:getSnapshot'),
  saveApiKey: (providerId: ProviderId, apiKey: string) => ipcRenderer.invoke('backend:saveApiKey', providerId, apiKey),
  connectSubscription: (providerId: ProviderId) => ipcRenderer.invoke('backend:connectSubscription', providerId),
  disconnectProvider: (providerId: ProviderId) => ipcRenderer.invoke('backend:disconnectProvider', providerId),
  launchOverlay: () => ipcRenderer.invoke('backend:launchOverlay'),
  stopOverlay: () => ipcRenderer.invoke('backend:stopOverlay'),
  refreshDesktopIntegration: () => ipcRenderer.invoke('backend:refreshDesktopIntegration'),
  installDesktopIntegration: () => ipcRenderer.invoke('backend:installDesktopIntegration'),
  setOverlayTheme: (theme: ResolvedAppearance) => ipcRenderer.invoke('backend:setOverlayTheme', theme),
  setPanelPosition: (position: PanelPosition) => ipcRenderer.invoke('backend:setPanelPosition', position),
  setPanelZoom: (zoom: PanelZoom) => ipcRenderer.invoke('backend:setPanelZoom', zoom),
  setPanelSize: (size: PanelSize) => ipcRenderer.invoke('backend:setPanelSize', size),
  setVoiceEnabled: (enabled: boolean) => ipcRenderer.invoke('backend:setVoiceEnabled', enabled),
  setVoiceInputDevice: (deviceId?: string) => ipcRenderer.invoke('backend:setVoiceInputDevice', deviceId),
  setVoiceInteractionMode: (mode: VoiceInteractionMode) => ipcRenderer.invoke('backend:setVoiceInteractionMode', mode),
  setVoiceSubmissionMode: (mode: VoiceSubmissionMode) => ipcRenderer.invoke('backend:setVoiceSubmissionMode', mode),
  refreshVoiceDevices: () => ipcRenderer.invoke('backend:refreshVoiceDevices'),
  retryVoiceSetup: () => ipcRenderer.invoke('backend:retryVoiceSetup'),
  setVoiceModel: (modelId: VoiceModelId) => ipcRenderer.invoke('backend:setVoiceModel', modelId),
  downloadVoiceModel: (modelId: VoiceModelId) => ipcRenderer.invoke('backend:downloadVoiceModel', modelId),
  cancelVoiceModelDownload: (modelId: VoiceModelId) => ipcRenderer.invoke('backend:cancelVoiceModelDownload', modelId),
  removeVoiceModel: (modelId: VoiceModelId) => ipcRenderer.invoke('backend:removeVoiceModel', modelId),
  startVoiceTest: () => ipcRenderer.invoke('backend:startVoiceTest'),
  stopVoiceTest: () => ipcRenderer.invoke('backend:stopVoiceTest'),
  shutdown: () => ipcRenderer.invoke('backend:shutdown'),
  onSnapshotChanged: (callback: (snapshot: BackendSnapshot) => void) => {
    const listener = (_event: Electron.IpcRendererEvent, snapshot: BackendSnapshot): void => callback(snapshot)
    ipcRenderer.on('backend:snapshotChanged', listener)
    return () => ipcRenderer.removeListener('backend:snapshotChanged', listener)
  },
  onVoiceLevelChanged: (callback: (level: number) => void) => {
    const listener = (_event: Electron.IpcRendererEvent, level: number): void => callback(level)
    ipcRenderer.on('backend:voiceLevelChanged', listener)
    return () => ipcRenderer.removeListener('backend:voiceLevelChanged', listener)
  },
  onPanelZoomChanged: (callback: (zoom: PanelZoom) => void) => {
    const listener = (_event: Electron.IpcRendererEvent, zoom: PanelZoom): void => callback(zoom)
    ipcRenderer.on('backend:panelZoomChanged', listener)
    return () => ipcRenderer.removeListener('backend:panelZoomChanged', listener)
  },
  onPanelSizeChanged: (callback: (size: PanelSize) => void) => {
    const listener = (_event: Electron.IpcRendererEvent, size: PanelSize): void => callback(size)
    ipcRenderer.on('backend:panelSizeChanged', listener)
    return () => ipcRenderer.removeListener('backend:panelSizeChanged', listener)
  }
})

contextBridge.exposeInMainWorld('chathead', Object.freeze({
  backend,
  window: Object.freeze({ minimize: () => ipcRenderer.send('window:minimize'), close: () => ipcRenderer.send('window:close') }),
  onUnavailable: (callback: (message: string) => void) => {
    const listener = (_event: Electron.IpcRendererEvent, message: string): void => callback(message)
    ipcRenderer.on('backend:unavailable', listener)
    return () => ipcRenderer.removeListener('backend:unavailable', listener)
  },
  onShowSettings: (callback: () => void) => {
    const listener = (): void => callback()
    ipcRenderer.on('window:showSettings', listener)
    return () => ipcRenderer.removeListener('window:showSettings', listener)
  }
}))
