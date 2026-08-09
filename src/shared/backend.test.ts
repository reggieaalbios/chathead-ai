import { describe, expect, it } from 'vitest'
import { PROTOCOL_VERSION } from './backend'

describe('desktop protocol', () => {
  it('uses version 2 for experimental chat snapshots and control events', () => {
    expect(PROTOCOL_VERSION).toBe(3)
  })
})
