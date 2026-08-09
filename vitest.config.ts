import { defineConfig } from 'vitest/config'
import react from '@vitejs/plugin-react'

export default defineConfig({
  plugins: [react()],
  test: { environment: 'jsdom', setupFiles: ['./src/renderer/src/test/setup.ts'] },
  resolve: { alias: { '@renderer': new URL('./src/renderer/src', import.meta.url).pathname, '@shared': new URL('./src/shared', import.meta.url).pathname } }
})

