import { tmpdir } from 'node:os'
import { resolve } from 'node:path'
import { beforeEach, expect, it, vi } from 'vitest'
import { registerAssetIndexTransferIPC } from './assetIndexTransfer'

const mock = vi.hoisted(() => ({
  handle: vi.fn(),
  save: vi.fn(),
  getVault: vi.fn(),
  current: vi.fn(),
  database: vi.fn(),
  build: vi.fn(),
  publish: vi.fn()
}))

vi.mock('electron', () => ({
  BrowserWindow: { fromWebContents: () => null },
  dialog: { showSaveDialog: mock.save },
  ipcMain: { handle: mock.handle }
}))
vi.mock('node:fs/promises', () => ({ realpath: async (path: string) => path }))
vi.mock('../sqliteDataBase', () => ({ getPublicDatabase: () => ({}) }))
vi.mock('../sqliteDataBase/VaultManager', () => ({
  VaultType: { NETWORK: 'network' },
  VaultManager: {
    getInstance: () => ({
      getVaultById: mock.getVault,
      getCurrentVault: mock.current,
      getCurrentVaultDatabase: mock.database
    })
  }
}))
vi.mock('../sqliteDataBase/models/assetIndexTransfer', () => ({
  buildAssetIndexTransfer: mock.build,
  InvalidAssetIndexTransferError: class extends Error {}
}))
vi.mock('./publishTransferFile', () => ({ publishTransferFile: mock.publish }))

const local = { id: 'a', name: 'Test library', path: '/test/a', vaultType: 'reference' }
const destination = resolve(tmpdir(), 'fixture-export.json')
const event = { sender: { isDestroyed: () => false } }

beforeEach(() => {
  vi.resetAllMocks()
  registerAssetIndexTransferIPC()
  mock.save.mockResolvedValue({ canceled: false, filePath: destination })
  mock.build.mockReturnValue({ content: '{}', folderCount: 0, assetCount: 0 })
})

it('exports just the selected registration if a different library is connected', async () => {
  mock.getVault.mockReturnValue(local)
  mock.current.mockReturnValue({ ...local, id: 'b' })
  const handler = mock.handle.mock.calls[0][1]
  await expect(handler(event, local.id)).resolves.toMatchObject({
    success: true,
    indexAvailable: false
  })
  expect(mock.database).not.toHaveBeenCalled()
  expect(mock.build).toHaveBeenCalledWith(local, null)
  expect(mock.publish).toHaveBeenCalledWith(destination, '{}')
})

it('rejects a selected library whose root changes during the save dialog', async () => {
  mock.getVault.mockReturnValueOnce(local).mockReturnValueOnce({ ...local, path: '/test/b' })
  const handler = mock.handle.mock.calls[0][1]
  await expect(handler(event, local.id)).resolves.toEqual({ success: false, code: 'invalid' })
  expect(mock.build).not.toHaveBeenCalled()
  expect(mock.publish).not.toHaveBeenCalled()
})
