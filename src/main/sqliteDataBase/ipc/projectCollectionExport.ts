import { BrowserWindow, dialog, ipcMain } from 'electron'
import { open } from 'node:fs/promises'
import { extname, isAbsolute } from 'node:path'
import { getPublicDatabase } from '../index'
import {
  buildProjectCollectionExport,
  InvalidProjectCollectionExportError
} from '../models/projectCollectionExport'
import type { ProjectCollectionsExportResult } from '../../../shared/projectCollectionsTransfer'

export function registerProjectCollectionExportIPC(): void {
  let exporting = false
  ipcMain.handle(
    'db:project:exportCollections',
    async (event): Promise<ProjectCollectionsExportResult> => {
      if (exporting) return { success: false, code: 'busy' }
      exporting = true
      try {
        const options: Electron.SaveDialogOptions = {
          defaultPath: 'uebox-project-collections.json',
          filters: [{ name: 'JSON', extensions: ['json'] }],
          properties: ['dontAddToRecent']
        }
        const parent = BrowserWindow.fromWebContents(event.sender)
        const selection = parent
          ? await dialog.showSaveDialog(parent, options)
          : await dialog.showSaveDialog(options)
        if (selection.canceled || !selection.filePath || event.sender.isDestroyed()) {
          return { success: false, code: 'cancelled' }
        }
        const destination = selection.filePath
        if (
          !isAbsolute(destination) ||
          destination.includes('\0') ||
          extname(destination).toLowerCase() !== '.json'
        ) {
          return { success: false, code: 'invalid' }
        }
        const { content, count } = buildProjectCollectionExport(
          getPublicDatabase(),
          process.platform === 'win32' ? 'windows' : 'posix'
        )
        const file = await open(destination, 'wx', 0o600)
        try {
          await file.writeFile(content, 'utf8')
          await file.sync()
        } finally {
          await file.close()
        }
        return { success: true, count }
      } catch (cause) {
        if (cause instanceof InvalidProjectCollectionExportError)
          return { success: false, code: 'invalid' }
        if (cause instanceof Error && 'code' in cause && cause.code === 'EEXIST') {
          return { success: false, code: 'exists' }
        }
        return { success: false, code: 'unavailable' }
      } finally {
        exporting = false
      }
    }
  )
}
