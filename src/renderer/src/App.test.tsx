import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { BackendSnapshot, PanelSize } from '../../shared/backend'
import { App } from './App'
import { useUiStore } from './store'

const zepOnlySnapshot: BackendSnapshot = {
  providers: [
    { id: 'chatgpt', name: 'ChatGPT', description: 'OpenAI', kind: 'largeLanguageModel', apiKeyLabel: 'OpenAI API key', supportsSubscription: true, status: { state: 'unconfigured' } },
    { id: 'zep', name: 'Zep', description: 'Memory', kind: 'memoryContext', apiKeyLabel: 'Zep API key', supportsSubscription: false, status: { state: 'authenticated', method: 'apiKey' } }
  ],
  launchReadiness: { ready: false, blockers: ['missingLaunchProvider'] },
  desktopIntegration: { kind: 'layerShell', status: 'ready' }, overlayRunning: false,
  voice: {
    enabled: false, phase: 'disabled', interactionMode: 'hold', submissionMode: 'insertOnly', inputDevices: [], microphoneAccess: 'unknown',
    selectedModelId: 'sherpa-onnx-whisper-tiny-int8-multilingual-v1', models: [
      { id: 'sherpa-onnx-whisper-tiny-int8-multilingual-v1', name: 'Whisper Tiny multilingual INT8', badges: ['Lightweight', 'Fastest'], description: 'Lower resource use for older hardware', languages: ['English', 'Filipino'], license: 'MIT', downloadSizeBytes: 116848715, installedSizeBytes: 104253757, resourceGuidance: 'Older hardware', state: 'notInstalled', downloadProgressPercent: 0, installedSizeBytesActual: 0 },
      { id: 'sherpa-onnx-qwen3-asr-0.6b-int8-2026-03-25', name: 'Qwen3-ASR 0.6B INT8', badges: ['Benchmark pending'], description: 'English, Filipino, and code-switching candidate', languages: ['English', 'Filipino'], license: 'Apache-2.0', downloadSizeBytes: 879346277, installedSizeBytes: 987659201, resourceGuidance: 'Four cores and 8 GB RAM', state: 'notInstalled', downloadProgressPercent: 0, installedSizeBytesActual: 0 }
    ],
    microphoneTestActive: false, message: 'Local voice is off.', recoverable: true
  },
  shortcutStatus: { state: 'registering' },
  panelShortcutStatus: { state: 'registering' },
  experimentalChat: { providerId: 'chatgpt', experimental: true, state: 'unavailable', message: 'Connect a ChatGPT subscription.' }
}

const getSnapshot = vi.fn(async () => zepOnlySnapshot)
const saveApiKey = vi.fn(async () => ({ ...zepOnlySnapshot, launchReadiness: { ready: true, blockers: [] } }))
const setPanelPosition = vi.fn(async () => zepOnlySnapshot)
const setPanelZoom = vi.fn(async () => zepOnlySnapshot)
const setPanelSize = vi.fn(async () => zepOnlySnapshot)
let panelZoomChanged: ((zoom: 80 | 90 | 100 | 110 | 125 | 150) => void) | undefined
let panelSizeChanged: ((size: PanelSize) => void) | undefined

beforeEach(() => {
  vi.clearAllMocks()
  localStorage.clear()
  panelZoomChanged = undefined
  panelSizeChanged = undefined
  useUiStore.setState({ snapshot: undefined, selectedProvider: undefined, unavailable: undefined, appearance: 'system', panelPosition: 'right', panelZoom: 100, panelSize: { width: 560, height: 460 }, view: 'home' })
  window.chathead = {
    backend: {
      getSnapshot, saveApiKey, connectSubscription: vi.fn(), disconnectProvider: vi.fn(), launchOverlay: vi.fn(), stopOverlay: vi.fn(),
      refreshDesktopIntegration: vi.fn(async () => zepOnlySnapshot), installDesktopIntegration: vi.fn(async () => zepOnlySnapshot),
      setOverlayTheme: vi.fn(async () => zepOnlySnapshot), setPanelPosition, setPanelZoom, setPanelSize,
      setVoiceEnabled: vi.fn(async () => zepOnlySnapshot), setVoiceInputDevice: vi.fn(async () => zepOnlySnapshot),
      setVoiceInteractionMode: vi.fn(async () => zepOnlySnapshot), setVoiceSubmissionMode: vi.fn(async () => zepOnlySnapshot), refreshVoiceDevices: vi.fn(async () => zepOnlySnapshot),
      retryVoiceSetup: vi.fn(async () => zepOnlySnapshot), setVoiceModel: vi.fn(async () => zepOnlySnapshot),
      downloadVoiceModel: vi.fn(async () => zepOnlySnapshot), cancelVoiceModelDownload: vi.fn(async () => zepOnlySnapshot), removeVoiceModel: vi.fn(async () => zepOnlySnapshot),
      startVoiceTest: vi.fn(async () => zepOnlySnapshot), stopVoiceTest: vi.fn(async () => zepOnlySnapshot),
      shutdown: vi.fn(), onSnapshotChanged: () => () => undefined, onVoiceLevelChanged: () => () => undefined,
      onPanelZoomChanged: (callback) => { panelZoomChanged = callback; return () => { panelZoomChanged = undefined } },
      onPanelSizeChanged: (callback) => { panelSizeChanged = callback; return () => { panelSizeChanged = undefined } }
    },
    window: { minimize: vi.fn(), close: vi.fn() }, onUnavailable: () => () => undefined, onShowSettings: () => () => undefined
  }
})

afterEach(cleanup)

describe('setup application', () => {
  it('keeps launch disabled when only Zep is authenticated', async () => {
    render(<App />)
    expect(await screen.findByRole('button', { name: 'Launch ChatHead' })).toBeDisabled()
  })

  it('offers a confirmed current-user install when GNOME integration is missing', async () => {
    vi.spyOn(window, 'confirm').mockReturnValueOnce(true)
    const gnomeSnapshot: BackendSnapshot = {
      ...zepOnlySnapshot,
      launchReadiness: { ready: false, blockers: ['desktopIntegrationRequired'] },
      desktopIntegration: { kind: 'gnomeShell', status: 'notInstalled', gnomeVersion: '46', message: 'Install the extension.' }
    }
    getSnapshot.mockResolvedValueOnce(gnomeSnapshot)
    vi.mocked(window.chathead.backend.installDesktopIntegration).mockResolvedValueOnce(gnomeSnapshot)
    render(<App />)
    fireEvent.click(await screen.findByRole('button', { name: 'Install' }))
    expect(window.confirm).toHaveBeenCalled()
    await waitFor(() => expect(window.chathead.backend.installDesktopIntegration).toHaveBeenCalled())
  })

  it('opens provider configuration and clears the key input after completion', async () => {
    saveApiKey.mockRejectedValueOnce(new Error('validation failed'))
    render(<App />)
    fireEvent.click(await screen.findByRole('button', { name: 'Configure ChatGPT' }))
    const input = screen.getByLabelText('OpenAI API key')
    fireEvent.change(input, { target: { value: 'sk-12345678901234567' } })
    fireEvent.click(screen.getByRole('button', { name: 'Save API key' }))
    await waitFor(() => expect(saveApiKey).toHaveBeenCalledWith('chatgpt', 'sk-12345678901234567'))
    await waitFor(() => expect(input).toHaveValue(''))
    expect(screen.getByRole('alert')).toHaveTextContent('validation failed')
  })

  it('hides subscription authentication when ChatGPT is already authenticated', async () => {
    getSnapshot.mockResolvedValueOnce({
      ...zepOnlySnapshot,
      providers: zepOnlySnapshot.providers.map((provider) => provider.id === 'chatgpt'
        ? { ...provider, status: { state: 'authenticated' as const, method: 'subscriptionLogin' as const } }
        : provider)
    })

    render(<App />)
    fireEvent.click(await screen.findByRole('button', { name: 'Configure ChatGPT' }))

    expect(screen.getByText('Connected with ChatGPT subscription')).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Connect with ChatGPT' })).not.toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Disconnect provider' })).toBeInTheDocument()
  })

  it('shows both authentication options for an unconfigured provider', async () => {
    getSnapshot.mockResolvedValueOnce({
      ...zepOnlySnapshot,
      providers: zepOnlySnapshot.providers.map((provider) => provider.id === 'zep'
        ? { ...provider, status: { state: 'unconfigured' as const } }
        : provider)
    })

    render(<App />)
    fireEvent.click(await screen.findByRole('button', { name: 'Configure Zep' }))

    expect(screen.getByRole('button', { name: 'Save API key' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Connect with Zep' })).toBeInTheDocument()
  })

  it('labels ChatGPT subscription chat as experimental', async () => {
    render(<App />)
    fireEvent.click(await screen.findByRole('button', { name: 'Configure ChatGPT' }))

    expect(screen.getByText(/subscription-backed chat runs locally/)).toHaveTextContent('Experimental:')
    expect(screen.getByRole('button', { name: 'Connect with ChatGPT (Experimental)' })).toBeInTheDocument()
  })

  it('reports an unwired subscription login without calling the backend', async () => {
    getSnapshot.mockResolvedValueOnce({
      ...zepOnlySnapshot,
      providers: zepOnlySnapshot.providers.map((provider) => provider.id === 'zep'
        ? { ...provider, status: { state: 'unconfigured' as const } }
        : provider)
    })

    render(<App />)
    fireEvent.click(await screen.findByRole('button', { name: 'Configure Zep' }))
    fireEvent.click(screen.getByRole('button', { name: 'Connect with Zep' }))

    expect(screen.getByRole('alert')).toHaveTextContent('Zep subscription login is not wired up yet.')
    expect(window.chathead.backend.connectSubscription).not.toHaveBeenCalled()
  })

  it('opens settings and applies the selected appearance', async () => {
    render(<App />)
    fireEvent.click(screen.getByRole('button', { name: 'Settings' }))
    const appearance = screen.getByRole('combobox', { name: 'Appearance' })
    expect(screen.getByRole('heading', { name: 'Preferences' })).toBeInTheDocument()
    fireEvent.click(appearance)
    fireEvent.click(screen.getByRole('option', { name: 'Dark' }))
    expect(appearance).toHaveTextContent('Dark')
    expect(document.documentElement).toHaveAttribute('data-theme', 'dark')
  })

  it('sets the native chat panel position from settings', async () => {
    render(<App />)
    fireEvent.click(screen.getByRole('button', { name: 'Settings' }))
    const position = screen.getByRole('combobox', { name: 'Chat panel position' })
    fireEvent.click(position)
    fireEvent.click(screen.getByRole('option', { name: 'Left of ChatHead' }))

    expect(position).toHaveTextContent('Left of ChatHead')
    expect(useUiStore.getState().panelPosition).toBe('left')
    await waitFor(() => expect(setPanelPosition).toHaveBeenLastCalledWith('left'))
  })

  it('replays persisted panel zoom and synchronizes native zoom events', async () => {
    localStorage.setItem('chathead-ui', JSON.stringify({
      state: { appearance: 'system', panelPosition: 'right', panelZoom: 125 },
      version: 1
    }))
    await useUiStore.persist.rehydrate()
    render(<App />)
    await waitFor(() => expect(setPanelZoom).toHaveBeenCalledWith(125))

    panelZoomChanged?.(90)
    await waitFor(() => expect(useUiStore.getState().panelZoom).toBe(90))
    expect(JSON.parse(localStorage.getItem('chathead-ui') ?? '{}').state.panelZoom).toBe(90)
  })

  it('falls back to 100 when persisted panel zoom is invalid', async () => {
    localStorage.setItem('chathead-ui', JSON.stringify({
      state: { appearance: 'system', panelPosition: 'right', panelZoom: 95 },
      version: 1
    }))
    await useUiStore.persist.rehydrate()
    expect(useUiStore.getState().panelZoom).toBe(100)
  })

  it('replays persisted panel size and synchronizes native resize events', async () => {
    localStorage.setItem('chathead-ui', JSON.stringify({
      state: { appearance: 'system', panelPosition: 'right', panelZoom: 100, panelSize: { width: 720, height: 600 } },
      version: 1
    }))
    await useUiStore.persist.rehydrate()
    render(<App />)
    await waitFor(() => expect(setPanelSize).toHaveBeenCalledWith({ width: 720, height: 600 }))

    panelSizeChanged?.({ width: 768, height: 568 })
    await waitFor(() => expect(useUiStore.getState().panelSize).toEqual({ width: 768, height: 568 }))
    expect(JSON.parse(localStorage.getItem('chathead-ui') ?? '{}').state.panelSize).toEqual({ width: 768, height: 568 })
  })

  it('falls back to the default when persisted panel size is invalid', async () => {
    localStorage.setItem('chathead-ui', JSON.stringify({
      state: { appearance: 'system', panelPosition: 'right', panelZoom: 100, panelSize: { width: 400, height: 900 } },
      version: 1
    }))
    await useUiStore.persist.rehydrate()
    expect(useUiStore.getState().panelSize).toEqual({ width: 560, height: 460 })
  })

  it('documents local and global shortcut groups without remapping controls', async () => {
    render(<App />)
    fireEvent.click(screen.getByRole('button', { name: 'Keyboard Shortcuts' }))

    expect(screen.getByRole('heading', { name: 'Interface' })).toBeInTheDocument()
    expect(screen.getByRole('heading', { name: 'Chat' })).toBeInTheDocument()
    expect(screen.getByRole('heading', { name: 'Global hotkeys' })).toBeInTheDocument()
    expect(screen.getAllByText('Panel-local')).toHaveLength(6)
    expect(screen.getAllByText('System-wide')).toHaveLength(3)
    expect(screen.getAllByText(/Registering · Waiting for the desktop portal/)).toHaveLength(2)
    expect(screen.queryByRole('combobox', { name: /shortcut/i })).not.toBeInTheDocument()
  })

  it.each([
    [{ state: 'ready' as const, trigger: 'Super+E' }, /Ready · Current shortcut: Super\+E/],
    [{ state: 'conflictPossible' as const, details: 'Already reserved.' }, /Conflict warning · Already reserved/],
    [{ state: 'unavailable' as const, details: 'Portal missing.' }, /Unavailable · Portal missing/]
  ])('renders the global shortcut status variant', async (shortcutStatus, expected) => {
    getSnapshot.mockResolvedValueOnce({ ...zepOnlySnapshot, shortcutStatus })
    render(<App />)
    fireEvent.click(screen.getByRole('button', { name: 'Keyboard Shortcuts' }))
    expect(await screen.findByText(expected)).toBeInTheDocument()
  })

  it('reports panel shortcut status independently from voice', async () => {
    getSnapshot.mockResolvedValueOnce({
      ...zepOnlySnapshot,
      shortcutStatus: { state: 'ready', trigger: 'Super+E' },
      panelShortcutStatus: { state: 'conflictPossible', details: 'Super+W is reserved.' }
    })
    render(<App />)
    fireEvent.click(screen.getByRole('button', { name: 'Keyboard Shortcuts' }))
    expect(await screen.findByText(/Ready · Current shortcut: Super\+E/)).toBeInTheDocument()
    expect(screen.getByText(/Conflict warning · Super\+W is reserved/)).toBeInTheDocument()
  })

  it('shows voice transcription without secondary setting descriptions', async () => {
    render(<App />)
    fireEvent.click(screen.getByRole('button', { name: 'Settings' }))
    expect(await screen.findByRole('heading', { name: 'Voice Transcription' })).toBeInTheDocument()
    expect(screen.queryByText(/Audio is never saved or uploaded/)).not.toBeInTheDocument()
    expect(screen.queryByText(/Hold Super\+E by default/)).not.toBeInTheDocument()
    expect(screen.getByRole('checkbox', { name: 'Enable voice transcription' })).not.toBeChecked()
  })

  it('opens the accessible model picker and offers independent downloads', async () => {
    render(<App />)
    fireEvent.click(screen.getByRole('button', { name: 'Settings' }))
    const picker = await screen.findByRole('button', { name: /Whisper Tiny multilingual INT8/ })
    fireEvent.click(picker)
    expect(picker).toHaveAttribute('aria-expanded', 'true')
    expect(screen.getByText('Qwen3-ASR 0.6B INT8')).toBeInTheDocument()
    expect(screen.getAllByRole('button', { name: 'Download' })).toHaveLength(2)
    fireEvent.keyDown(document, { key: 'Escape' })
    expect(picker).toHaveAttribute('aria-expanded', 'false')
  })
})
