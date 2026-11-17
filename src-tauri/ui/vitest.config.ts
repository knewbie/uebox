import { defineConfig } from 'vitest/config'
import vue from '@vitejs/plugin-vue'
import { fileURLToPath, URL } from 'node:url'

export default defineConfig({
  plugins: [vue()],
  resolve: {
    alias: {
      '@renderer': fileURLToPath(new URL('../../src/renderer/src', import.meta.url))
    }
  },
  test: {
    environment: 'happy-dom',
    include: ['src-tauri/ui/src/**/*.test.ts', 'src-tauri/ui/tests/**/*.test.ts']
  }
})
