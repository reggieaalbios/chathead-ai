import { z } from 'zod'

export const PROTOCOL_VERSION = 11 as const

export const providerIds = ['chatgpt', 'claude', 'gemini', 'grok', 'zep'] as const
export type ProviderId = (typeof providerIds)[number]
export type ResolvedAppearance = 'light' | 'dark'
export type PanelPosition = 'left' | 'right'
export const panelZoomLevels = [80, 90, 100, 110, 125, 150] as const
export type PanelZoom = (typeof panelZoomLevels)[number]
export interface PanelSize { width: number; height: number }
export const defaultPanelSize: PanelSize = { width: 560, height: 460 }
export type ErrorCode =
  | 'INVALID_API_KEY'
  | 'CREDENTIAL_STORE_UNAVAILABLE'
  | 'CODEX_NOT_FOUND'
  | 'AUTH_FAILED'
  | 'DESKTOP_INTEGRATION_REQUIRED'
  | 'DESKTOP_INTEGRATION_UNAVAILABLE'
  | 'SIDECAR_UNAVAILABLE'
  | 'PROTOCOL_MISMATCH'
  | 'INVALID_REQUEST'
  | 'UNSUPPORTED_OPERATION'
  | 'CODEX_PROTOCOL_ERROR'
  | 'CHAT_UNAVAILABLE'
  | 'CHAT_BUSY'
  | 'VOICE_UNAVAILABLE'
  | 'VOICE_SETUP_FAILED'
  | 'VOICE_INVALID_STATE'

export type VoiceInteractionMode = 'hold' | 'toggle'
export type VoiceSubmissionMode = 'insertOnly' | 'insertAndSend'
export const voiceModelIds = ['sherpa-onnx-whisper-tiny-int8-multilingual-v1', 'sherpa-onnx-qwen3-asr-0.6b-int8-2026-03-25'] as const
export type VoiceModelId = (typeof voiceModelIds)[number]
export type VoicePhase = 'disabled' | 'setupRequired' | 'downloading' | 'loading' | 'ready' | 'listening' | 'transcribing' | 'pendingSend' | 'error'
export interface VoiceInputDevice { id: string; name: string; isDefault: boolean }
export interface VoiceModelSnapshot {
  id: VoiceModelId
  name: string
  badges: string[]
  description: string
  languages: string[]
  license: string
  downloadSizeBytes: number
  installedSizeBytes: number
  resourceGuidance: string
  state: 'notInstalled' | 'downloading' | 'installed' | 'invalid'
  downloadProgressPercent: number
  installedSizeBytesActual: number
  error?: string
}
export interface VoiceSnapshot {
  enabled: boolean
  phase: VoicePhase
  interactionMode: VoiceInteractionMode
  submissionMode: VoiceSubmissionMode
  selectedInputDeviceId?: string
  defaultInputDeviceId?: string
  inputDevices: VoiceInputDevice[]
  microphoneAccess: 'unknown' | 'granted' | 'denied' | 'unavailable'
  selectedModelId: VoiceModelId
  activeModelId?: VoiceModelId
  models: VoiceModelSnapshot[]
  microphoneTestActive: boolean
  message?: string
  recoverable: boolean
}

export interface BackendError { code: ErrorCode; message: string; recoverable: boolean }
export type ProviderStatus =
  | { state: 'unconfigured' }
  | { state: 'authenticating' }
  | { state: 'authenticated'; method: 'apiKey' | 'subscriptionLogin' }
  | { state: 'error' | 'unavailable'; message: string }

export interface ProviderSnapshot {
  id: ProviderId
  name: string
  description: string
  kind: 'largeLanguageModel' | 'memoryContext'
  apiKeyLabel: string
  supportsSubscription: boolean
  status: ProviderStatus
}

export interface BackendSnapshot {
  providers: ProviderSnapshot[]
  launchReadiness: {
    ready: boolean
    blockers: Array<'missingLaunchProvider' | 'desktopIntegrationRequired' | 'desktopIntegrationUnavailable'>
  }
  desktopIntegration: {
    kind: 'layerShell' | 'gnomeShell' | 'unsupported'
    status: 'ready' | 'notInstalled' | 'disabled' | 'incompatible' | 'unavailable'
    gnomeVersion?: string
    message?: string
  }
  overlayRunning: boolean
  voice: VoiceSnapshot
  shortcutStatus:
    | { state: 'registering' }
    | { state: 'ready'; trigger: string }
    | { state: 'conflictPossible' | 'unavailable'; details: string }
  panelShortcutStatus:
    | { state: 'registering' }
    | { state: 'ready'; trigger: string }
    | { state: 'conflictPossible' | 'unavailable'; details: string }
  experimentalChat: {
    providerId: 'chatgpt'
    experimental: true
    state: 'probing' | 'authenticating' | 'ready' | 'unavailable' | 'error'
    message?: string
  }
}

export interface BackendApi {
  getSnapshot(): Promise<BackendSnapshot>
  saveApiKey(providerId: ProviderId, apiKey: string): Promise<BackendSnapshot>
  connectSubscription(providerId: ProviderId): Promise<BackendSnapshot>
  disconnectProvider(providerId: ProviderId): Promise<BackendSnapshot>
  launchOverlay(): Promise<BackendSnapshot>
  stopOverlay(): Promise<BackendSnapshot>
  refreshDesktopIntegration(): Promise<BackendSnapshot>
  installDesktopIntegration(): Promise<BackendSnapshot>
  setOverlayTheme(theme: ResolvedAppearance): Promise<BackendSnapshot>
  setPanelPosition(position: PanelPosition): Promise<BackendSnapshot>
  setPanelZoom(zoom: PanelZoom): Promise<BackendSnapshot>
  setPanelSize(size: PanelSize): Promise<BackendSnapshot>
  setVoiceEnabled(enabled: boolean): Promise<BackendSnapshot>
  setVoiceInputDevice(deviceId?: string): Promise<BackendSnapshot>
  setVoiceInteractionMode(mode: VoiceInteractionMode): Promise<BackendSnapshot>
  setVoiceSubmissionMode(mode: VoiceSubmissionMode): Promise<BackendSnapshot>
  refreshVoiceDevices(): Promise<BackendSnapshot>
  retryVoiceSetup(): Promise<BackendSnapshot>
  setVoiceModel(modelId: VoiceModelId): Promise<BackendSnapshot>
  downloadVoiceModel(modelId: VoiceModelId): Promise<BackendSnapshot>
  cancelVoiceModelDownload(modelId: VoiceModelId): Promise<BackendSnapshot>
  removeVoiceModel(modelId: VoiceModelId): Promise<BackendSnapshot>
  startVoiceTest(): Promise<BackendSnapshot>
  stopVoiceTest(): Promise<BackendSnapshot>
  shutdown(): Promise<void>
  onSnapshotChanged(callback: (snapshot: BackendSnapshot) => void): () => void
  onVoiceLevelChanged(callback: (level: number) => void): () => void
  onPanelZoomChanged(callback: (zoom: PanelZoom) => void): () => void
  onPanelSizeChanged(callback: (size: PanelSize) => void): () => void
}

export const panelZoomSchema = z.union([
  z.literal(80), z.literal(90), z.literal(100), z.literal(110), z.literal(125), z.literal(150)
])

export const panelSizeSchema = z.object({
  width: z.number().int().min(420).max(960),
  height: z.number().int().min(460).max(800)
}).strict()

const providerStatusSchema = z.discriminatedUnion('state', [
  z.object({ state: z.literal('unconfigured') }),
  z.object({ state: z.literal('authenticating') }),
  z.object({ state: z.literal('authenticated'), method: z.enum(['apiKey', 'subscriptionLogin']) }),
  z.object({ state: z.literal('error'), message: z.string() }),
  z.object({ state: z.literal('unavailable'), message: z.string() })
])

export const voiceSnapshotSchema = z.object({
  enabled: z.boolean(),
  phase: z.enum(['disabled', 'setupRequired', 'downloading', 'loading', 'ready', 'listening', 'transcribing', 'pendingSend', 'error']),
  interactionMode: z.enum(['hold', 'toggle']),
  submissionMode: z.enum(['insertOnly', 'insertAndSend']),
  selectedInputDeviceId: z.string().optional(),
  defaultInputDeviceId: z.string().optional(),
  inputDevices: z.array(z.object({ id: z.string(), name: z.string(), isDefault: z.boolean() })),
  microphoneAccess: z.enum(['unknown', 'granted', 'denied', 'unavailable']),
  selectedModelId: z.enum(voiceModelIds),
  activeModelId: z.enum(voiceModelIds).optional(),
  models: z.array(z.object({
    id: z.enum(voiceModelIds), name: z.string(), badges: z.array(z.string()), description: z.string(),
    languages: z.array(z.string()), license: z.string(), downloadSizeBytes: z.number().int().nonnegative(),
    installedSizeBytes: z.number().int().nonnegative(), resourceGuidance: z.string(),
    state: z.enum(['notInstalled', 'downloading', 'installed', 'invalid']),
    downloadProgressPercent: z.number().int().min(0).max(100),
    installedSizeBytesActual: z.number().int().nonnegative(), error: z.string().optional()
  })).length(2),
  microphoneTestActive: z.boolean(),
  message: z.string().optional(),
  recoverable: z.boolean()
})

export const backendSnapshotSchema = z.object({
  providers: z.array(z.object({
    id: z.enum(providerIds), name: z.string(), description: z.string(), kind: z.enum(['largeLanguageModel', 'memoryContext']),
    apiKeyLabel: z.string(), supportsSubscription: z.boolean(), status: providerStatusSchema
  })),
  launchReadiness: z.object({
    ready: z.boolean(),
    blockers: z.array(z.enum(['missingLaunchProvider', 'desktopIntegrationRequired', 'desktopIntegrationUnavailable']))
  }),
  desktopIntegration: z.object({
    kind: z.enum(['layerShell', 'gnomeShell', 'unsupported']),
    status: z.enum(['ready', 'notInstalled', 'disabled', 'incompatible', 'unavailable']),
    gnomeVersion: z.string().optional(),
    message: z.string().optional()
  }),
  overlayRunning: z.boolean(),
  voice: voiceSnapshotSchema,
  shortcutStatus: z.union([
    z.object({ state: z.literal('registering') }),
    z.object({ state: z.literal('ready'), trigger: z.string() }),
    z.object({ state: z.literal('conflictPossible'), details: z.string() }),
    z.object({ state: z.literal('unavailable'), details: z.string() })
  ]),
  panelShortcutStatus: z.union([
    z.object({ state: z.literal('registering') }),
    z.object({ state: z.literal('ready'), trigger: z.string() }),
    z.object({ state: z.literal('conflictPossible'), details: z.string() }),
    z.object({ state: z.literal('unavailable'), details: z.string() })
  ]),
  experimentalChat: z.object({
    providerId: z.literal('chatgpt'), experimental: z.literal(true),
    state: z.enum(['probing', 'authenticating', 'ready', 'unavailable', 'error']), message: z.string().optional()
  })
})
