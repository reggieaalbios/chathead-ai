import { create } from 'zustand'
import { persist } from 'zustand/middleware'
import { defaultPanelSize, panelSizeSchema, panelZoomLevels, type BackendSnapshot, type PanelSize, type PanelZoom, type ProviderId } from '../../shared/backend'
import type { PanelPosition } from '../../shared/backend'

export type Appearance = 'system' | 'light' | 'dark'
export type AppView = 'home' | 'settings' | 'shortcuts'

interface UiState {
  snapshot?: BackendSnapshot
  selectedProvider?: ProviderId
  unavailable?: string
  appearance: Appearance
  panelPosition: PanelPosition
  panelZoom: PanelZoom
  panelSize: PanelSize
  view: AppView
  setSnapshot(snapshot: BackendSnapshot): void
  selectProvider(providerId?: ProviderId): void
  setUnavailable(message?: string): void
  setAppearance(appearance: Appearance): void
  setPanelPosition(panelPosition: PanelPosition): void
  setPanelZoom(panelZoom: PanelZoom): void
  setPanelSize(panelSize: PanelSize): void
  setView(view: AppView): void
}

export const useUiStore = create<UiState>()(persist<UiState, [], [], Pick<UiState, 'appearance' | 'panelPosition' | 'panelZoom' | 'panelSize'>>((set) => ({
  appearance: 'system',
  panelPosition: 'right',
  panelZoom: 100,
  panelSize: defaultPanelSize,
  view: 'home',
  setSnapshot: (snapshot) => set({ snapshot, unavailable: undefined }),
  selectProvider: (selectedProvider) => set({ selectedProvider }),
  setUnavailable: (unavailable) => set({ unavailable }),
  setAppearance: (appearance) => set({ appearance }),
  setPanelPosition: (panelPosition) => set({ panelPosition }),
  setPanelZoom: (panelZoom) => set({ panelZoom }),
  setPanelSize: (panelSize) => set((state) => state.panelSize.width === panelSize.width && state.panelSize.height === panelSize.height ? state : { panelSize }),
  setView: (view) => set({ view, selectedProvider: undefined })
}), {
  name: 'chathead-ui',
  version: 1,
  merge: (persisted, current) => {
    const saved = persisted as Partial<UiState>
    const parsedPanelSize = panelSizeSchema.safeParse(saved.panelSize)
    return {
      ...current,
      ...saved,
      panelZoom: panelZoomLevels.includes(saved.panelZoom as PanelZoom) ? saved.panelZoom as PanelZoom : 100,
      panelSize: parsedPanelSize.success ? parsedPanelSize.data : defaultPanelSize
    }
  },
  partialize: ({ appearance, panelPosition, panelZoom, panelSize }) => ({ appearance, panelPosition, panelZoom, panelSize })
}))
