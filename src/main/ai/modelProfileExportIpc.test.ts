/** @vitest-environment node */
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { registerModelProfileExportIPC } from './modelProfileExportIpc'

const fixture = vi.hoisted(() => ({
  getPath: vi.fn(),
  handle: vi.fn(),
  showSaveDialog: vi.fn()
}))

vi.mock('electron', () => ({
  app: { getPath: fixture.getPath },
  ipcMain: { handle: fixture.handle },
  BrowserWindow: { fromWebContents: vi.fn() },
  dialog: { showSaveDialog: fixture.showSaveDialog }
}))

let root: string
let destination: string

beforeEach(async () => {
  vi.clearAllMocks()
  root = await mkdtemp(join(tmpdir(), 'uebox-profile-transfer-'))
  destination = join(root, 'public-model-list.json')
  fixture.getPath.mockReturnValue(root)
  fixture.showSaveDialog.mockResolvedValue({ canceled: false, filePath: destination })
  registerModelProfileExportIPC()
})

afterEach(async () => {
  await rm(root, { recursive: true, force: true })
})

async function request(): Promise<unknown> {
  const handler = fixture.handle.mock.calls.find(
    ([command]) => command === 'ai-provider:export-model-profiles'
  )?.[1] as ((event: { sender: { isDestroyed: () => boolean } }) => Promise<unknown>) | undefined
  expect(handler).toBeTypeOf('function')
  return handler?.({ sender: { isDestroyed: () => false } })
}

it('exports only public model metadata and preserves the encrypted store and source config', async () => {
  const source = JSON.stringify({
    version: 2,
    providers: [
      {
        id: 'fixture',
        displayName: 'Fixture',
        kind: 'chat',
        protocol: 'openai-completions',
        baseUrl: 'https://example.test/v1',
        models: [{ id: 'fixture-model' }],
        apiKey: { kind: 'literal', id: 'private-reference', value: 'private-key' },
        headers: { authorization: 'private-header' }
      }
    ],
    roles: {}
  })
  await writeFile(join(root, 'models.json'), source)
  await writeFile(join(root, 'ai-provider-secrets.bin'), 'opaque-fixture')
  await expect(request()).resolves.toMatchObject({ success: true, profiles: 1 })
  const written = await readFile(destination, 'utf8')
  expect(written).toContain('"requiresReauthorization":true')
  expect(written).not.toContain('private-')
  expect(await readFile(join(root, 'models.json'), 'utf8')).toBe(source)
  expect(await readFile(join(root, 'ai-provider-secrets.bin'), 'utf8')).toBe('opaque-fixture')
  await expect(request()).resolves.toMatchObject({ code: 'model_profile_export_exists' })
  expect(await readFile(destination, 'utf8')).toBe(written)
})

it('leaves the source unread when the native save dialog is cancelled', async () => {
  fixture.showSaveDialog.mockResolvedValue({ canceled: true })
  await expect(request()).resolves.toMatchObject({ code: 'model_profile_export_cancelled' })
  expect(fixture.getPath).not.toHaveBeenCalled()
})
