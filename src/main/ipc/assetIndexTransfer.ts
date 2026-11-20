import { BrowserWindow, dialog, ipcMain } from 'electron'
import { realpath } from 'node:fs/promises'
import { basename, dirname, extname, isAbsolute, join } from 'node:path'
import type { AssetIndexExportReply } from '../../shared/assetIndexTransfer'
import { getPublicDatabase } from '../sqliteDataBase'
import { VaultManager, VaultType } from '../sqliteDataBase/VaultManager'
import {
  buildAssetIndexTransfer,
  InvalidAssetIndexTransferError
} from '../sqliteDataBase/models/assetIndexTransfer'
import { publishTransferFile } from './publishTransferFile'

export function registerAssetIndexTransferIPC(): void {
  let exporting = false
  ipcMain.handle(
    'vault:export-local-asset-index',
    async (event, vaultId: unknown): Promise<AssetIndexExportReply> => {
      if (exporting) return { success: false, code: 'busy' }
      if (typeof vaultId !== 'string' || !vaultId || Buffer.byteLength(vaultId) > 256) {
        return { success: false, code: 'invalid' }
      }
      exporting = true
      try {
        const manager = VaultManager.getInstance(getPublicDatabase())
        const initial = manager.getVaultById(vaultId)
        if (!initial || initial.vaultType === VaultType.NETWORK) {
          return { success: false, code: 'invalid' }
        }

        const options: Electron.SaveDialogOptions = {
          defaultPath: 'uebox-asset-index.json',
          filters: [{ name: 'JSON', extensions: ['json'] }],
          properties: ['dontAddToRecent']
        }
        const window = BrowserWindow.fromWebContents(event.sender)
        const selected = window
          ? await dialog.showSaveDialog(window, options)
          : await dialog.showSaveDialog(options)
        if (selected.canceled || !selected.filePath || event.sender.isDestroyed()) {
          return { success: false, code: 'cancelled' }
        }
        if (
          !isAbsolute(selected.filePath) ||
          extname(selected.filePath).toLowerCase() !== '.json'
        ) {
          return { success: false, code: 'invalid' }
        }
        const parent = await realpath(dirname(selected.filePath))
        const vault = manager.getVaultById(vaultId)
        if (!vault || vault.vaultType !== initial.vaultType || vault.path !== initial.path) {
          return { success: false, code: 'invalid' }
        }
        const database =
          manager.getCurrentVault()?.id === vault.id ? manager.getCurrentVaultDatabase() : null
        const exported = buildAssetIndexTransfer(vault, database)
        await publishTransferFile(join(parent, basename(selected.filePath)), exported.content)
        return {
          success: true,
          folderCount: exported.folderCount,
          assetCount: exported.assetCount,
          indexAvailable: database !== null
        }
      } catch (error) {
        if (error instanceof InvalidAssetIndexTransferError)
          return { success: false, code: 'invalid' }
        if (error instanceof Error && 'code' in error && error.code === 'EEXIST') {
          return { success: false, code: 'exists' }
        }
        return { success: false, code: 'unavailable' }
      } finally {
        exporting = false
      }
    }
  )
}
