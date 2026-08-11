import { useEffect, useMemo, useRef, useState } from 'react'
import { Activity, Check, ChevronDown, Home, Keyboard, LoaderCircle, Minus, Settings, X } from 'lucide-react'
import { z } from 'zod'
import type { BackendSnapshot, ProviderId, ProviderSnapshot, VoiceInteractionMode, VoiceModelId, VoiceSubmissionMode } from '../../shared/backend'
import type { PanelPosition } from '../../shared/backend'
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
  const { snapshot, selectedProvider, unavailable, appearance, panelPosition, panelZoom, panelSize, view, setSnapshot, selectProvider, setUnavailable, setAppearance, setPanelPosition, setPanelZoom, setPanelSize, setView } = useUiStore()
  const [busy, setBusy] = useState(false)
  const [systemDark, setSystemDark] = useState(() => window.matchMedia?.('(prefers-color-scheme: dark)').matches ?? false)
  const selected = useMemo(() => snapshot?.providers.find((provider) => provider.id === selectedProvider), [snapshot, selectedProvider])
  const resolvedAppearance = appearance === 'system' ? (systemDark ? 'dark' : 'light') : appearance

  useEffect(() => {
    void window.chathead.backend.getSnapshot().then(setSnapshot).catch((error: Error) => setUnavailable(error.message))
    const removeSnapshot = window.chathead.backend.onSnapshotChanged(setSnapshot)
    const removePanelZoom = window.chathead.backend.onPanelZoomChanged(setPanelZoom)
    const removePanelSize = window.chathead.backend.onPanelSizeChanged(setPanelSize)
    const removeUnavailable = window.chathead.onUnavailable(setUnavailable)
    const removeShowSettings = window.chathead.onShowSettings(() => setView('settings'))
    return () => { removeSnapshot(); removePanelZoom(); removePanelSize(); removeUnavailable(); removeShowSettings() }
  }, [setPanelSize, setPanelZoom, setSnapshot, setUnavailable, setView])

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
    void window.chathead.backend.setPanelPosition(panelPosition).catch((error: Error) => setUnavailable(error.message))
  }, [panelPosition, setUnavailable])

  useEffect(() => {
    void window.chathead.backend.setPanelZoom(panelZoom).catch((error: Error) => setUnavailable(error.message))
  }, [panelZoom, setUnavailable])

  useEffect(() => {
    void window.chathead.backend.setPanelSize(panelSize).catch((error: Error) => setUnavailable(error.message))
  }, [panelSize, setUnavailable])

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
        <NavButton label="Keyboard Shortcuts" active={view === 'shortcuts'} onClick={() => setView('shortcuts')}><Keyboard size={20} /></NavButton>
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
        </> : view === 'settings'
          ? <SettingsView snapshot={snapshot} appearance={appearance} panelPosition={panelPosition} onError={setUnavailable} onAppearanceChange={setAppearance} onPanelPositionChange={setPanelPosition} />
          : <KeyboardShortcutsView snapshot={snapshot} />}
      </section>

      {selected && <ProviderModal provider={selected} theme={resolvedAppearance} onClose={() => selectProvider(undefined)} onSnapshot={setSnapshot} />}
    </main>
  )
}

function KeyboardShortcutsView({ snapshot }: { snapshot?: BackendSnapshot }): React.JSX.Element {
  const voiceStatus = describeGlobalShortcut(snapshot?.shortcutStatus, 'Super+E')
  const panelStatus = describeGlobalShortcut(snapshot?.panelShortcutStatus, 'Super+W')
  const interactionMode = snapshot?.voice.interactionMode === 'toggle' ? 'Toggle' : 'Hold to talk'

  return (
    <section className="shortcuts-view" aria-labelledby="shortcuts-title">
      <h1 id="shortcuts-title">Keyboard Shortcuts</h1>
      <ShortcutGroup title="Interface" entries={[
        { action: 'Zoom in', keys: 'Ctrl + + / Ctrl + wheel up', scope: 'Panel-local' },
        { action: 'Zoom out', keys: 'Ctrl + - / Ctrl + wheel down', scope: 'Panel-local' },
        { action: 'Reset zoom', keys: 'Ctrl + 0', scope: 'Panel-local' }
      ]} />
      <ShortcutGroup title="Chat" entries={[
        { action: 'Send message', keys: 'Enter', scope: 'Panel-local' },
        { action: 'Insert newline', keys: 'Shift + Enter', scope: 'Panel-local' },
        { action: 'Cancel voice', keys: 'Escape', scope: 'Panel-local' }
      ]} />
      <ShortcutGroup title="Global hotkeys" entries={[
        { action: 'Voice shortcut', keys: 'Preferred: Super+E', scope: 'System-wide', status: `${voiceStatus.label} · ${voiceStatus.detail}` },
        { action: 'Toggle chat panel', keys: 'Preferred: Super+W', scope: 'System-wide', status: `${panelStatus.label} · ${panelStatus.detail}` },
        { action: 'Interaction mode', keys: interactionMode, scope: 'System-wide' }
      ]} />
    </section>
  )
}

function describeGlobalShortcut(shortcut: BackendSnapshot['shortcutStatus'] | undefined, preferred: string): { label: string; detail: string } {
  return !shortcut || shortcut.state === 'registering'
    ? { label: 'Registering', detail: `Waiting for the desktop portal. Preferred shortcut: ${preferred}.` }
    : shortcut.state === 'ready'
      ? { label: 'Ready', detail: `Current shortcut: ${shortcut.trigger}` }
      : shortcut.state === 'conflictPossible'
        ? { label: 'Conflict warning', detail: shortcut.details }
        : { label: 'Unavailable', detail: shortcut.details }
}

interface ShortcutEntry { action: string; keys: string; scope: 'Panel-local' | 'System-wide'; status?: string }

function ShortcutGroup({ title, entries }: { title: string; entries: ShortcutEntry[] }): React.JSX.Element {
  return (
    <section className="shortcut-group" aria-labelledby={`shortcut-${title.toLowerCase().replaceAll(' ', '-')}`}>
      <h2 id={`shortcut-${title.toLowerCase().replaceAll(' ', '-')}`}>{title}</h2>
      <div className="shortcut-list">
        {entries.map((entry) => <div className="shortcut-row" key={entry.action}>
          <div><strong>{entry.action}</strong>{entry.status && <small>{entry.status}</small>}</div>
          <div className="shortcut-binding"><kbd>{entry.keys}</kbd><span>{entry.scope}</span></div>
        </div>)}
      </div>
    </section>
  )
}

function NavButton({ label, active = false, onClick, children }: { label: string; active?: boolean; onClick?: () => void; children: React.ReactNode }): React.JSX.Element {
  return <button className={`nav-button ${active ? 'active' : ''}`} aria-label={label} title={label} onClick={onClick}>{children}</button>
}

function SettingsView({ snapshot, appearance, panelPosition, onError, onAppearanceChange, onPanelPositionChange }: {
  snapshot?: BackendSnapshot
  appearance: Appearance
  panelPosition: PanelPosition
  onError(message: string): void
  onAppearanceChange(appearance: Appearance): void
  onPanelPositionChange(position: PanelPosition): void
}): React.JSX.Element {
  const appearanceOptions: Array<{ value: Appearance; label: string }> = [
    { value: 'system', label: 'System' },
    { value: 'light', label: 'Light' },
    { value: 'dark', label: 'Dark' }
  ]
  const positionOptions: Array<{ value: PanelPosition; label: string }> = [
    { value: 'left', label: 'Left of ChatHead' },
    { value: 'right', label: 'Right of ChatHead' }
  ]

  return (
    <section className="settings-view" aria-labelledby="settings-title">
      <h1 id="settings-title">Preferences</h1>
      <div className="settings-list">
        <div className="settings-row">
          <span>Appearance</span>
          <SettingSelect label="Appearance" value={appearance} options={appearanceOptions} onChange={onAppearanceChange} />
        </div>
        <div className="settings-row">
          <span>Chat panel position</span>
          <SettingSelect label="Chat panel position" value={panelPosition} options={positionOptions} onChange={onPanelPositionChange} />
        </div>
      </div>
      {snapshot && <LocalVoiceSettings snapshot={snapshot} onError={onError} />}
    </section>
  )
}

function LocalVoiceSettings({ snapshot, onError }: { snapshot: BackendSnapshot; onError(message: string): void }): React.JSX.Element {
  const voice = snapshot.voice
  const [level, setLevel] = useState(0)
  const [busy, setBusy] = useState(false)
  const [modelsOpen, setModelsOpen] = useState(false)
  const [removeCandidate, setRemoveCandidate] = useState<VoiceModelId>()
  const deviceOptions = [
    { value: '', label: 'System default' },
    ...voice.inputDevices.map((device) => ({ value: device.id, label: device.name }))
  ]
  const modeOptions: Array<{ value: VoiceInteractionMode; label: string }> = [
    { value: 'hold', label: 'Hold to talk' },
    { value: 'toggle', label: 'Toggle' }
  ]
  const submissionOptions: Array<{ value: VoiceSubmissionMode; label: string }> = [
    { value: 'insertOnly', label: 'Insert text only' },
    { value: 'insertAndSend', label: 'Insert and send' }
  ]

  useEffect(() => window.chathead.backend.onVoiceLevelChanged(setLevel), [])
  useEffect(() => {
    if (!modelsOpen) return
    const close = (event: KeyboardEvent): void => { if (event.key === 'Escape') setModelsOpen(false) }
    document.addEventListener('keydown', close)
    return () => document.removeEventListener('keydown', close)
  }, [modelsOpen])

  async function run(action: () => Promise<BackendSnapshot>): Promise<void> {
    setBusy(true)
    try { await action() }
    catch (error) { onError(error instanceof Error ? error.message : String(error)) }
    finally { setBusy(false) }
  }

  const selectedModel = voice.models.find((model) => model.id === voice.selectedModelId)
  const modelActionsDisabled = busy || voice.microphoneTestActive || ['listening', 'transcribing', 'pendingSend', 'loading'].includes(voice.phase)
  return (
    <section className="local-voice" aria-labelledby="local-voice-title">
      <div className="settings-section-heading">
        <h2 id="local-voice-title">Voice Transcription</h2>
        <label className="voice-toggle"><input type="checkbox" checked={voice.enabled} disabled={busy} onChange={(event) => void run(() => window.chathead.backend.setVoiceEnabled(event.target.checked))} /><span>Enable voice transcription</span></label>
      </div>
      <div className="settings-row compact voice-model-row-setting">
        <span>Model</span>
        <div className="voice-model-picker">
          <button className="voice-model-trigger" aria-expanded={modelsOpen} aria-controls="voice-model-list" onClick={() => setModelsOpen((open) => !open)}>
            <strong>{selectedModel?.name ?? 'Choose a local model'}</strong>
            <ChevronDown className={modelsOpen ? 'open' : ''} size={16} />
          </button>
          {selectedModel?.state === 'downloading' && <progress max="100" value={selectedModel.downloadProgressPercent}>{selectedModel.downloadProgressPercent}%</progress>}
          {modelsOpen && <div id="voice-model-list" className="voice-model-list" role="list">
            {voice.models.map((model) => {
              const active = voice.activeModelId === model.id
              return <article className="voice-model-row" role="listitem" key={model.id}>
                <div className="voice-model-heading"><strong>{model.name}</strong><span>{model.badges.map((badge) => <small key={badge}>{badge}</small>)}</span></div>
                {model.state === 'downloading' && <div className="voice-model-progress"><progress max="100" value={model.downloadProgressPercent}>{model.downloadProgressPercent}%</progress><span>{model.downloadProgressPercent}%</span></div>}
                {model.error && <p className="voice-error">{model.error}</p>}
                <div className="voice-model-actions">
                  {model.state === 'notInstalled' && <button disabled={busy} onClick={() => void run(() => window.chathead.backend.downloadVoiceModel(model.id))}>Download</button>}
                  {model.state === 'downloading' && <button disabled={busy} onClick={() => void run(() => window.chathead.backend.cancelVoiceModelDownload(model.id))}>Cancel</button>}
                  {(model.state === 'invalid' || model.error) && model.state !== 'downloading' && <button disabled={busy} onClick={() => void run(() => window.chathead.backend.downloadVoiceModel(model.id))}>Retry download</button>}
                  {model.state === 'installed' && !active && <button disabled={modelActionsDisabled} onClick={() => void run(() => window.chathead.backend.setVoiceModel(model.id))}>Use model</button>}
                  {active && <span className="voice-active">Active</span>}
                  {model.state === 'installed' && !active && <button disabled={modelActionsDisabled} onClick={() => setRemoveCandidate(model.id)}>Remove</button>}
                  {model.state === 'installed' && active && <button disabled={voice.enabled || modelActionsDisabled} onClick={() => setRemoveCandidate(model.id)}>Remove</button>}
                </div>
              </article>
            })}
          </div>}
        </div>
      </div>
      {voice.enabled && voice.message && <div className="settings-row compact voice-status"><span>Status</span><span className={voice.phase === 'error' ? 'voice-error' : 'voice-message'}>{voice.message}</span></div>}
      <div className="settings-row compact">
        <span>Microphone</span>
        <SettingSelect label="Voice microphone" value={voice.selectedInputDeviceId ?? ''} options={deviceOptions} onChange={(value) => void run(() => window.chathead.backend.setVoiceInputDevice(value || undefined))} />
      </div>
      <div className="settings-row compact">
        <span>Activation</span>
        <SettingSelect label="Voice shortcut behavior" value={voice.interactionMode} options={modeOptions} onChange={(mode) => void run(() => window.chathead.backend.setVoiceInteractionMode(mode))} />
      </div>
      <div className="settings-row compact">
        <span>After transcription</span>
        <SettingSelect label="After voice transcription" value={voice.submissionMode} options={submissionOptions} onChange={(mode) => void run(() => window.chathead.backend.setVoiceSubmissionMode(mode))} />
      </div>
      <div className="voice-test-row">
        <button disabled={busy || !voice.enabled || voice.phase !== 'ready'} onClick={() => void run(() => voice.microphoneTestActive ? window.chathead.backend.stopVoiceTest() : window.chathead.backend.startVoiceTest())}>{voice.microphoneTestActive ? 'Stop microphone test' : 'Test microphone (10s)'}</button>
        <div className="voice-meter" role="meter" aria-label="Microphone level" aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(level * 100)}><span style={{ width: `${Math.min(100, level * 180)}%` }} /></div>
        <button disabled={busy} onClick={() => void run(() => window.chathead.backend.refreshVoiceDevices())}>Refresh</button>
      </div>
      <div className="voice-actions">
        {(voice.phase === 'error' || voice.phase === 'setupRequired') && <button disabled={busy || !voice.enabled} onClick={() => void run(() => window.chathead.backend.retryVoiceSetup())}>Retry setup</button>}
      </div>
      {removeCandidate && <div className="voice-dialog-backdrop"><div className="voice-dialog" role="dialog" aria-modal="true" aria-labelledby="remove-model-title"><h3 id="remove-model-title">Remove local model?</h3><p>This removes only the selected model files. Other downloaded models are unchanged.</p><div><button onClick={() => setRemoveCandidate(undefined)}>Keep model</button><button onClick={() => { const modelId = removeCandidate; setRemoveCandidate(undefined); void run(() => window.chathead.backend.removeVoiceModel(modelId)) }}>Remove model</button></div></div></div>}
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
