import { describe, expect, it } from 'vitest'
import { PROTOCOL_VERSION, backendSnapshotSchema, panelSizeSchema, panelZoomSchema } from './backend'

describe('desktop protocol', () => {
  it('uses version 10 for voice submission mode and bounded panel settings', () => {
    expect(PROTOCOL_VERSION).toBe(10)
    expect(panelZoomSchema.safeParse(125).success).toBe(true)
    expect(panelZoomSchema.safeParse(95).success).toBe(false)
    expect(panelSizeSchema.safeParse({ width: 420, height: 460 }).success).toBe(true)
    expect(panelSizeSchema.safeParse({ width: 960, height: 800 }).success).toBe(true)
    expect(panelSizeSchema.safeParse({ width: 419, height: 460 }).success).toBe(false)
    expect(panelSizeSchema.safeParse({ width: 560, height: 801 }).success).toBe(false)
  })

  it('accepts the Rust camel-case local voice snapshot shape', () => {
    expect(backendSnapshotSchema.safeParse({
      providers: [], launchReadiness: 'missingLaunchProvider', overlayRunning: false,
      voice: {
        enabled: false, phase: 'disabled', interactionMode: 'hold', submissionMode: 'insertOnly', inputDevices: [], microphoneAccess: 'unknown',
        selectedModelId: 'sherpa-onnx-whisper-tiny-int8-multilingual-v1', models: [
          { id: 'sherpa-onnx-whisper-tiny-int8-multilingual-v1', name: 'Whisper', badges: [], description: 'Small', languages: ['English'], license: 'MIT', downloadSizeBytes: 1, installedSizeBytes: 1, resourceGuidance: 'Low', state: 'notInstalled', downloadProgressPercent: 0, installedSizeBytesActual: 0 },
          { id: 'sherpa-onnx-qwen3-asr-0.6b-int8-2026-03-25', name: 'Qwen', badges: [], description: 'Large', languages: ['Filipino'], license: 'Apache-2.0', downloadSizeBytes: 2, installedSizeBytes: 2, resourceGuidance: 'High', state: 'notInstalled', downloadProgressPercent: 0, installedSizeBytesActual: 0 }
        ], microphoneTestActive: false, recoverable: true
      },
      shortcutStatus: { state: 'registering' },
      panelShortcutStatus: { state: 'registering' },
      experimentalChat: { providerId: 'chatgpt', experimental: true, state: 'unavailable' }
    }).success).toBe(true)
  })

  it('rejects an out-of-range model progress value', () => {
    expect(backendSnapshotSchema.safeParse({
      providers: [], launchReadiness: 'missingLaunchProvider', overlayRunning: false,
      voice: {
        enabled: true, phase: 'downloading', interactionMode: 'hold', submissionMode: 'insertOnly', inputDevices: [], microphoneAccess: 'unknown',
        selectedModelId: 'sherpa-onnx-whisper-tiny-int8-multilingual-v1', models: [
          { id: 'sherpa-onnx-whisper-tiny-int8-multilingual-v1', name: 'Whisper', badges: [], description: 'Small', languages: [], license: 'MIT', downloadSizeBytes: 1, installedSizeBytes: 1, resourceGuidance: 'Low', state: 'downloading', downloadProgressPercent: 101, installedSizeBytesActual: 0 },
          { id: 'sherpa-onnx-qwen3-asr-0.6b-int8-2026-03-25', name: 'Qwen', badges: [], description: 'Large', languages: [], license: 'Apache', downloadSizeBytes: 2, installedSizeBytes: 2, resourceGuidance: 'High', state: 'notInstalled', downloadProgressPercent: 0, installedSizeBytesActual: 0 }
        ], microphoneTestActive: false, recoverable: true
      },
      shortcutStatus: { state: 'registering' },
      panelShortcutStatus: { state: 'registering' },
      experimentalChat: { providerId: 'chatgpt', experimental: true, state: 'unavailable' }
    }).success).toBe(false)
  })
})
