import { readFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { describe, expect, it } from 'vitest'

const extensionDirectory = resolve('gnome-extension/chathead-ai@io.github.chathead-ai')

describe('GNOME 46 extension bundle', () => {
  it('declares only the live-tested target and no deprecated metadata version', async () => {
    const metadata = JSON.parse(await readFile(resolve(extensionDirectory, 'metadata.json'), 'utf8')) as Record<string, unknown>
    expect(metadata.uuid).toBe('chathead-ai@io.github.chathead-ai')
    expect(metadata['shell-version']).toEqual(['46'])
    expect(metadata).not.toHaveProperty('version')
    expect(metadata).not.toHaveProperty('session-modes')
  })

  it('uses GNOME 46 actor APIs and includes complete teardown', async () => {
    const source = await readFile(resolve(extensionDirectory, 'extension.js'), 'utf8')
    expect(source).toContain("resource:///org/gnome/shell/extensions/extension.js")
    expect(source).toContain('Main.layoutManager.addTopChrome')
    expect(source).toContain('add_child')
    expect(source).toContain('object.disconnect(id)')
    expect(source).toContain('this._root?.destroy()')
    expect(source).not.toMatch(/\b(?:add_actor|remove_actor)\s*\(/)
    expect(source).not.toContain('Clutter.DragAction')
    expect(source).not.toContain('new Clutter.PanAction')
    expect(source).toContain("this._orb.connect('button-press-event'")
    expect(source).toContain("global.stage.connect('captured-event'")
    expect(source).toContain('Clutter.EventType.MOTION')
    expect(source).toContain('Clutter.EventType.TOUCH_UPDATE')
    expect(source).toContain('const type = event.type()')
    expect(source).not.toContain('event.get_type()')
    expect(source).toContain("this.dir.get_child('chathead-orb.svg')")
    expect(source).not.toContain('extensionUtils')
    expect(source).not.toMatch(/XWayland|global\.get_window_actors|Meta\.Barrier\.display/)
  })

  it('bundles the native-style orb artwork', async () => {
    const artwork = await readFile(resolve(extensionDirectory, 'chathead-orb.svg'), 'utf8')
    expect(artwork).toContain('<svg')
    expect(artwork).toContain('orb-shell')
    expect(artwork).toContain('face-plate')
    const installer = await readFile(resolve('src/main/index.ts'), 'utf8')
    const packageCheck = await readFile(resolve('scripts/check-gnome-extension.sh'), 'utf8')
    expect(installer).toContain("'--extra-source', 'chathead-orb.svg', '.'")
    expect(installer).toContain('], source)')
    expect(packageCheck).toContain('--extra-source chathead-orb.svg .')
  })

  it('does not log presentation or security-sensitive content', async () => {
    const source = await readFile(resolve(extensionDirectory, 'extension.js'), 'utf8')
    expect(source).not.toMatch(/console\.(?:log|debug|info)/)
    expect(source).not.toMatch(/credential|apiKey|token|rawAudio|modelFile|threadId/)
  })

  it('returns the single readiness D-Bus output as a scalar', async () => {
    const source = await readFile(resolve(extensionDirectory, 'extension.js'), 'utf8')
    const readinessMethod = source.match(/GetReadiness\(\) \{([\s\S]*?)\n {4}\}/)?.[1]
    expect(readinessMethod).toContain('return JSON.stringify(')
    expect(readinessMethod).not.toContain('return [')
  })
})
