import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { BackendSnapshot } from '../../shared/backend'
import { App } from './App'
import { useUiStore } from './store'

const zepOnlySnapshot: BackendSnapshot = {
  providers: [
    { id: 'chatgpt', name: 'ChatGPT', description: 'OpenAI', kind: 'largeLanguageModel', apiKeyLabel: 'OpenAI API key', supportsSubscription: true, status: { state: 'unconfigured' } },
    { id: 'zep', name: 'Zep', description: 'Memory', kind: 'memoryContext', apiKeyLabel: 'Zep API key', supportsSubscription: false, status: { state: 'authenticated', method: 'apiKey' } }
  ],
  launchReadiness: 'missingLaunchProvider', overlayRunning: false, voiceState: 'idle', shortcutStatus: { state: 'registering' },
  experimentalChat: { providerId: 'chatgpt', experimental: true, state: 'unavailable', message: 'Connect a ChatGPT subscription.' }
}

const getSnapshot = vi.fn(async () => zepOnlySnapshot)
const saveApiKey = vi.fn(async () => ({ ...zepOnlySnapshot, launchReadiness: 'ready' as const }))
const setOverlayPosition = vi.fn(async () => zepOnlySnapshot)

beforeEach(() => {
  vi.clearAllMocks()
  localStorage.clear()
  useUiStore.setState({ snapshot: undefined, selectedProvider: undefined, unavailable: undefined, appearance: 'system', overlayPosition: 'left', view: 'home' })
  window.chathead = {
    backend: { getSnapshot, saveApiKey, connectSubscription: vi.fn(), disconnectProvider: vi.fn(), launchOverlay: vi.fn(), stopOverlay: vi.fn(), setOverlayTheme: vi.fn(async () => zepOnlySnapshot), setOverlayPosition, shutdown: vi.fn(), onSnapshotChanged: () => () => undefined },
    window: { minimize: vi.fn(), close: vi.fn() }, onUnavailable: () => () => undefined
  }
})

afterEach(cleanup)

describe('setup application', () => {
  it('keeps launch disabled when only Zep is authenticated', async () => {
    render(<App />)
    expect(await screen.findByRole('button', { name: 'Launch ChatHead' })).toBeDisabled()
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
    expect(screen.getByRole('heading', { name: 'General' })).toBeInTheDocument()
    fireEvent.click(appearance)
    fireEvent.click(screen.getByRole('option', { name: 'Dark' }))
    expect(appearance).toHaveTextContent('Dark')
    expect(document.documentElement).toHaveAttribute('data-theme', 'dark')
  })

  it('sets the native ChatHead position from settings', async () => {
    render(<App />)
    fireEvent.click(screen.getByRole('button', { name: 'Settings' }))
    const position = screen.getByRole('combobox', { name: 'ChatHead position' })
    fireEvent.click(position)
    fireEvent.click(screen.getByRole('option', { name: 'Right' }))

    expect(position).toHaveTextContent('Right')
    expect(useUiStore.getState().overlayPosition).toBe('right')
    await waitFor(() => expect(setOverlayPosition).toHaveBeenLastCalledWith('right'))
  })
})
