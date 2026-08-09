import { create } from 'zustand'
import { persist } from 'zustand/middleware'
import type { BackendSnapshot, ProviderId } from '../../shared/backend'
import type { OverlayPosition } from '../../shared/backend'

export type Appearance = 'system' | 'light' | 'dark'
export type AppView = 'home' | 'settings'

interface UiState {
  snapshot?: BackendSnapshot
  selectedProvider?: ProviderId
  unavailable?: string
  appearance: Appearance
  overlayPosition: OverlayPosition
  view: AppView
  setSnapshot(snapshot: BackendSnapshot): void
  selectProvider(providerId?: ProviderId): void
  setUnavailable(message?: string): void
  setAppearance(appearance: Appearance): void
  setOverlayPosition(overlayPosition: OverlayPosition): void
  setView(view: AppView): void
}

export const useUiStore = create<UiState>()(persist<UiState, [], [], Pick<UiState, 'appearance' | 'overlayPosition'>>((set) => ({
  appearance: 'system',
  overlayPosition: 'left',
  view: 'home',
  setSnapshot: (snapshot) => set({ snapshot, unavailable: undefined }),
  selectProvider: (selectedProvider) => set({ selectedProvider }),
  setUnavailable: (unavailable) => set({ unavailable }),
  setAppearance: (appearance) => set({ appearance }),
  setOverlayPosition: (overlayPosition) => set({ overlayPosition }),
  setView: (view) => set({ view, selectedProvider: undefined })
}), {
  name: 'chathead-ui',
  partialize: ({ appearance, overlayPosition }) => ({ appearance, overlayPosition })
}))
