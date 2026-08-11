import type { BackendApi } from '../../shared/backend'

declare global {
  interface Window {
    chathead: {
      backend: BackendApi
      window: { minimize(): void; close(): void }
      onUnavailable(callback: (message: string) => void): () => void
      onShowSettings(callback: () => void): () => void
    }
  }
}

export {}
