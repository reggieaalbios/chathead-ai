import { describe, expect, it } from 'vitest'
import { approvedCodexLoginUrl } from './external'

describe('Codex login URL validation', () => {
  it('allows only approved HTTPS origins', () => {
    expect(approvedCodexLoginUrl('https://chatgpt.com/auth')).toBe('https://chatgpt.com/auth')
    expect(approvedCodexLoginUrl('https://auth.openai.com/oauth/authorize')).toBe('https://auth.openai.com/oauth/authorize')
    expect(approvedCodexLoginUrl('http://chatgpt.com/auth')).toBeUndefined()
    expect(approvedCodexLoginUrl('https://chatgpt.com.evil.example/auth')).toBeUndefined()
    expect(approvedCodexLoginUrl('https://user:password@chatgpt.com/auth')).toBeUndefined()
  })
})
