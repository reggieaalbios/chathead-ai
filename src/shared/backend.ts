export const PROTOCOL_VERSION = 3 as const

export const providerIds = ['chatgpt', 'claude', 'gemini', 'grok', 'zep'] as const
export type ProviderId = (typeof providerIds)[number]
export type ResolvedAppearance = 'light' | 'dark'
export type OverlayPosition = 'left' | 'right'
export type ErrorCode =
  | 'INVALID_API_KEY'
  | 'CREDENTIAL_STORE_UNAVAILABLE'
  | 'CODEX_NOT_FOUND'
  | 'AUTH_FAILED'
  | 'LAYER_SHELL_UNSUPPORTED'
  | 'SIDECAR_UNAVAILABLE'
  | 'PROTOCOL_MISMATCH'
  | 'INVALID_REQUEST'
  | 'UNSUPPORTED_OPERATION'
  | 'CODEX_PROTOCOL_ERROR'
  | 'CHAT_UNAVAILABLE'
  | 'CHAT_BUSY'

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
  launchReadiness: 'ready' | 'missingLaunchProvider'
  overlayRunning: boolean
  voiceState: 'idle' | 'listening'
  shortcutStatus:
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
  setOverlayTheme(theme: ResolvedAppearance): Promise<BackendSnapshot>
  setOverlayPosition(position: OverlayPosition): Promise<BackendSnapshot>
  shutdown(): Promise<void>
  onSnapshotChanged(callback: (snapshot: BackendSnapshot) => void): () => void
}
