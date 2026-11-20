import { BrowserWindow, dialog, ipcMain } from 'electron'
import { basename, dirname, extname, isAbsolute, join } from 'node:path'
import { realpath } from 'node:fs/promises'
import type { NoteTransferReply } from '../../shared/noteTransfer'
import { getPublicDatabase } from '../sqliteDataBase'
import { buildNoteTransfer, InvalidNoteTransferError } from '../sqliteDataBase/models/noteTransfer'
import { publishTransferFile } from './publishTransferFile'

export function registerNoteTransferIPC(): void {
  let exporting = false
  ipcMain.handle('notebook:export-offline-notes', async (event): Promise<NoteTransferReply> => {
    if (exporting) return { success: false, code: 'busy' }
    exporting = true
    try {
      const options: Electron.SaveDialogOptions = {
        defaultPath: 'uebox-offline-notes.json',
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
      if (!isAbsolute(selected.filePath) || extname(selected.filePath).toLowerCase() !== '.json') {
        return { success: false, code: 'invalid' }
      }

      const parent = await realpath(dirname(selected.filePath))
      const destination = join(parent, basename(selected.filePath))
      const exported = buildNoteTransfer(getPublicDatabase())
      await publishTransferFile(destination, exported.content)
      return {
        success: true,
        count: exported.count,
        skipped: exported.skipped
      }
    } catch (error) {
      if (error instanceof InvalidNoteTransferError) return { success: false, code: 'invalid' }
      if (error instanceof Error && 'code' in error && error.code === 'EEXIST') {
        return { success: false, code: 'exists' }
      }
      return { success: false, code: 'unavailable' }
    } finally {
      exporting = false
    }
  })
}
