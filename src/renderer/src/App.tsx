import { useEffect, useMemo, useRef, useState } from 'react'
import { Activity, Check, ChevronDown, Home, LoaderCircle, Minus, Settings, X } from 'lucide-react'
import { z } from 'zod'
import type { ProviderId, ProviderSnapshot } from '../../shared/backend'
import type { OverlayPosition } from '../../shared/backend'
import type { Appearance } from './store'
import { useUiStore } from './store'

const apiKeySchema = z.string().trim().min(1, 'Enter an API key.')
const providerArtwork: Record<ProviderId, Record<'light' | 'dark', string>> = {
  chatgpt: { light: '/light/image-Photoroom%20(1).png', dark: '/dark/image-Photoroom%20(7).png' },
  claude: { light: '/image-Photoroom%20(5).png', dark: '/image-Photoroom%20(5).png' },
  gemini: { light: '/image-Photoroom.png', dark: '/image-Photoroom.png' },
  grok: { light: '/light/image-Photoroom%20(2).png', dark: '/dark/image-Photoroom%20(6).png' },
  zep: { light: '/image-Photoroom%20(4).png', dark: '/image-Photoroom%20(4).png' }
}

export function App(): React.JSX.Element {
  const { snapshot, selectedProvider, unavailable, appearance, overlayPosition, view, setSnapshot, selectProvider, setUnavailable, setAppearance, setOverlayPosition, setView } = useUiStore()
  const [busy, setBusy] = useState(false)
  const [systemDark, setSystemDark] = useState(() => window.matchMedia?.('(prefers-color-scheme: dark)').matches ?? false)
  const selected = useMemo(() => snapshot?.providers.find((provider) => provider.id === selectedProvider), [snapshot, selectedProvider])
  const resolvedAppearance = appearance === 'system' ? (systemDark ? 'dark' : 'light') : appearance

  useEffect(() => {
    void window.chathead.backend.getSnapshot().then(setSnapshot).catch((error: Error) => setUnavailable(error.message))
    const removeSnapshot = window.chathead.backend.onSnapshotChanged(setSnapshot)
    const removeUnavailable = window.chathead.onUnavailable(setUnavailable)
    return () => { removeSnapshot(); removeUnavailable() }
  }, [setSnapshot, setUnavailable])

  useEffect(() => {
    const media = window.matchMedia?.('(prefers-color-scheme: dark)')
    if (!media) return
    const update = (event: MediaQueryListEvent): void => setSystemDark(event.matches)
    media.addEventListener('change', update)
    return () => media.removeEventListener('change', update)
  }, [])

  useEffect(() => {
    document.documentElement.dataset.theme = resolvedAppearance
    document.documentElement.style.colorScheme = resolvedAppearance
    void window.chathead.backend.setOverlayTheme(resolvedAppearance).catch((error: Error) => setUnavailable(error.message))
  }, [resolvedAppearance, setUnavailable])

  useEffect(() => {
    void window.chathead.backend.setOverlayPosition(overlayPosition).catch((error: Error) => setUnavailable(error.message))
  }, [overlayPosition, setUnavailable])

  async function toggleOverlay(): Promise<void> {
    setBusy(true)
    try {
      const next = snapshot?.overlayRunning ? await window.chathead.backend.stopOverlay() : await window.chathead.backend.launchOverlay()
      setSnapshot(next)
    } catch (error) { setUnavailable(error instanceof Error ? error.message : String(error)) }
    finally { setBusy(false) }
  }

  const launchDisabled = busy || Boolean(unavailable) || (!snapshot?.overlayRunning && snapshot?.launchReadiness !== 'ready')

  return (
    <main className="app-shell" data-theme={resolvedAppearance}>
      <aside className="sidebar" aria-label="Primary navigation">
        <NavButton label="Home" active={view === 'home'} onClick={() => setView('home')}><Home size={20} /></NavButton>
        <NavButton label="Settings" active={view === 'settings'} onClick={() => setView('settings')}><Settings size={20} /></NavButton>
        <NavButton label="Activity"><Activity size={20} /></NavButton>
      </aside>

      <section className="workspace">
        <header className="titlebar">
          <div className="window-controls">
            <button aria-label="Minimize" onClick={() => window.chathead.window.minimize()}><Minus size={16} /></button>
            <button aria-label="Close" onClick={() => window.chathead.window.close()}><X size={16} /></button>
          </div>
        </header>

        {view === 'home' ? <>
          <div className="launch-zone">
            <button className="launch-button" disabled={launchDisabled} onClick={() => void toggleOverlay()}>
              {busy && <LoaderCircle className="spin" size={16} />}
              {snapshot?.overlayRunning ? 'Stop ChatHead' : 'Launch ChatHead'}
            </button>
            <p>{snapshot?.launchReadiness === 'ready' ? 'Your assistant is ready.' : 'Connect at least one AI provider to launch.'}</p>
          </div>

          {unavailable && <div className="backend-error" role="alert">{unavailable}</div>}

          <section className="providers" aria-label="AI providers">
            <div className="provider-grid">
              {snapshot?.providers.map((provider) => (
                <ProviderTile key={provider.id} provider={provider} theme={resolvedAppearance} onClick={() => selectProvider(provider.id)} />
              ))}
              {!snapshot && Array.from({ length: 5 }, (_, index) => <div className="provider-skeleton" key={index} />)}
            </div>
          </section>
        </> : <SettingsView appearance={appearance} overlayPosition={overlayPosition} onAppearanceChange={setAppearance} onOverlayPositionChange={setOverlayPosition} />}
      </section>

      {selected && <ProviderModal provider={selected} theme={resolvedAppearance} onClose={() => selectProvider(undefined)} onSnapshot={setSnapshot} />}
    </main>
  )
}

function NavButton({ label, active = false, onClick, children }: { label: string; active?: boolean; onClick?: () => void; children: React.ReactNode }): React.JSX.Element {
  return <button className={`nav-button ${active ? 'active' : ''}`} aria-label={label} title={label} onClick={onClick}>{children}</button>
}

function SettingsView({ appearance, overlayPosition, onAppearanceChange, onOverlayPositionChange }: {
  appearance: Appearance
  overlayPosition: OverlayPosition
  onAppearanceChange(appearance: Appearance): void
  onOverlayPositionChange(position: OverlayPosition): void
}): React.JSX.Element {
  const appearanceOptions: Array<{ value: Appearance; label: string }> = [
    { value: 'system', label: 'System' },
    { value: 'light', label: 'Light' },
    { value: 'dark', label: 'Dark' }
  ]
  const positionOptions: Array<{ value: OverlayPosition; label: string }> = [
    { value: 'left', label: 'Left' },
    { value: 'right', label: 'Right' }
  ]

  return (
    <section className="settings-view" aria-labelledby="settings-title">
      <h1 id="settings-title">General</h1>
      <div className="settings-list">
        <div className="settings-row">
          <span>Appearance</span>
          <SettingSelect label="Appearance" value={appearance} options={appearanceOptions} onChange={onAppearanceChange} />
        </div>
        <div className="settings-row">
          <span>ChatHead position</span>
          <SettingSelect label="ChatHead position" value={overlayPosition} options={positionOptions} onChange={onOverlayPositionChange} />
        </div>
      </div>
    </section>
  )
}

function SettingSelect<T extends string>({ label, value, options, onChange }: {
  label: string
  value: T
  options: Array<{ value: T; label: string }>
  onChange(value: T): void
}): React.JSX.Element {
  const [open, setOpen] = useState(false)
  const dropdownRef = useRef<HTMLDivElement>(null)
  const optionListId = `${label.toLowerCase().replaceAll(' ', '-')}-options`
  const selectedLabel = options.find((option) => option.value === value)?.label ?? options[0]?.label

  useEffect(() => {
    if (!open) return
    const closeOnOutsideClick = (event: MouseEvent): void => {
      if (!dropdownRef.current?.contains(event.target as Node)) setOpen(false)
    }
    const closeOnEscape = (event: KeyboardEvent): void => {
      if (event.key === 'Escape') setOpen(false)
    }
    document.addEventListener('mousedown', closeOnOutsideClick)
    document.addEventListener('keydown', closeOnEscape)
    return () => {
      document.removeEventListener('mousedown', closeOnOutsideClick)
      document.removeEventListener('keydown', closeOnEscape)
    }
  }, [open])

  return (
    <div className="appearance-select" ref={dropdownRef}>
      <button className="appearance-trigger" type="button" role="combobox" aria-label={label} aria-controls={optionListId} aria-expanded={open} onClick={() => setOpen((current) => !current)}>
        <span>{selectedLabel}</span>
        <ChevronDown className={open ? 'open' : ''} size={16} />
      </button>
      {open && (
        <div className="appearance-menu" id={optionListId} role="listbox" aria-label={`${label} options`}>
          {options.map((option) => (
            <button key={option.value} type="button" className={option.value === value ? 'selected' : ''} role="option" aria-selected={option.value === value} onClick={() => { onChange(option.value); setOpen(false) }}>
              <span>{option.label}</span>
              {option.value === value && <Check size={15} />}
            </button>
          ))}
        </div>
      )}
    </div>
  )
}

function ProviderTile({ provider, theme, onClick }: { provider: ProviderSnapshot; theme: 'light' | 'dark'; onClick(): void }): React.JSX.Element {
  const authenticated = provider.status.state === 'authenticated'
  return (
    <button className="provider-tile" onClick={onClick} aria-label={`Configure ${provider.name}`}>
      <span className={`provider-art ${authenticated ? 'connected' : ''}`}><img src={providerArtwork[provider.id][theme]} alt="" /></span>
      <span className="provider-name">{provider.name}</span>
      <span className={`status-pill ${authenticated ? 'ok' : ''}`}>{authenticated ? 'Connected' : 'Setup'}</span>
    </button>
  )
}

function ProviderModal({ provider, theme, onClose, onSnapshot }: { provider: ProviderSnapshot; theme: 'light' | 'dark'; onClose(): void; onSnapshot(snapshot: Awaited<ReturnType<typeof window.chathead.backend.getSnapshot>>): void }): React.JSX.Element {
  const [apiKey, setApiKey] = useState('')
  const [busy, setBusy] = useState<'key' | 'subscription' | 'disconnect'>()
  const [error, setError] = useState<string>()
  const authenticated = provider.status.state === 'authenticated'

  async function saveKey(event: React.FormEvent): Promise<void> {
    event.preventDefault()
    const parsed = apiKeySchema.safeParse(apiKey)
    if (!parsed.success) { setError(parsed.error.issues[0]?.message); return }
    setBusy('key'); setError(undefined)
    try {
      onSnapshot(await window.chathead.backend.saveApiKey(provider.id, parsed.data))
      setApiKey('')
      onClose()
    }
    catch (caught) { setError(caught instanceof Error ? caught.message : String(caught)) }
    finally { setApiKey(''); setBusy(undefined) }
  }

  async function connect(): Promise<void> {
    if (!provider.supportsSubscription) {
      setError(`${provider.name} subscription login is not wired up yet.`)
      return
    }
    setBusy('subscription'); setError(undefined)
    try { onSnapshot(await window.chathead.backend.connectSubscription(provider.id)); onClose() }
    catch (caught) { setError(caught instanceof Error ? caught.message : String(caught)) }
    finally { setBusy(undefined) }
  }

  async function disconnect(): Promise<void> {
    setBusy('disconnect'); setError(undefined)
    try { onSnapshot(await window.chathead.backend.disconnectProvider(provider.id)); onClose() }
    catch (caught) { setError(caught instanceof Error ? caught.message : String(caught)) }
    finally { setBusy(undefined) }
  }

  return (
    <div className="modal-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) onClose() }}>
      <section className="provider-modal" role="dialog" aria-modal="true" aria-labelledby="provider-title">
        <button className="modal-close" aria-label="Close provider settings" onClick={onClose}><X size={18} /></button>
        <div className="modal-heading">
          <span className="provider-art"><img src={providerArtwork[provider.id][theme]} alt="" /></span>
          <div><h1 id="provider-title">Connect {provider.name}</h1><p>{provider.description}</p></div>
        </div>

        <div className={`connection-state ${authenticated ? 'connected' : ''}`}>
          <span />{provider.status.state === 'authenticated' ? `Connected with ${provider.status.method === 'apiKey' ? 'API key' : 'ChatGPT subscription'}` : 'Not connected'}
        </div>

        {provider.id === 'chatgpt' && <p className="security-note"><strong>Experimental:</strong> subscription-backed chat runs locally through the installed Codex app-server and is not intended as a production integration.</p>}

        <form onSubmit={(event) => void saveKey(event)}>
          <label htmlFor="api-key">{provider.apiKeyLabel}</label>
          <input id="api-key" type="password" autoComplete="off" value={apiKey} disabled={Boolean(busy)} onChange={(event) => setApiKey(event.target.value)} placeholder="Paste your key securely" />
          <button className="primary-action" type="submit" disabled={Boolean(busy)}>
            {busy === 'key' && <LoaderCircle className="spin" size={15} />}
            Save API key
          </button>
        </form>

        {!authenticated && <div className="divider"><span>or</span></div>}
        {!authenticated && <button className="secondary-action" disabled={Boolean(busy)} onClick={() => void connect()}>{busy === 'subscription' && <LoaderCircle className="spin" size={15} />} Connect with {provider.name}{provider.id === 'chatgpt' ? ' (Experimental)' : ''}</button>}
        {authenticated && <button className="disconnect-action" disabled={Boolean(busy)} onClick={() => void disconnect()}>{busy === 'disconnect' && <LoaderCircle className="spin" size={15} />} Disconnect provider</button>}
        {error && <p className="modal-error" role="alert">{error}</p>}
        <p className="security-note">Credentials go directly to the native secret store and are never retained by this interface.</p>
      </section>
    </div>
  )
}
