const allowedCodexLoginHosts = new Set(['chatgpt.com', 'auth.openai.com'])

export function approvedCodexLoginUrl(value: string): string | undefined {
  try {
    const url = new URL(value)
    if (url.protocol !== 'https:' || !allowedCodexLoginHosts.has(url.hostname)) return undefined
    if (url.username || url.password) return undefined
    return url.toString()
  } catch {
    return undefined
  }
}
