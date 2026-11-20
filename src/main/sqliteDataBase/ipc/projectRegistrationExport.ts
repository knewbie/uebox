import { BrowserWindow, dialog, ipcMain } from 'electron'
import { open } from 'node:fs/promises'
import { isAbsolute } from 'node:path'
import { getPublicDatabase } from '../index'
import {
  buildProjectRegistrationExport,
  InvalidProjectRegistrationError
} from '../models/projectRegistrationExport'

import type { ProjectRegistrationExportResult } from '../../../shared/projectRegistration'

function failure(
  code: Extract<ProjectRegistrationExportResult, { success: false }>['code']
): ProjectRegistrationExportResult {
  return { success: false, code }
}

async function writeExclusive(filePath: string, content: string): Promise<void> {
  // Never open a second database, preflight with exists(), or truncate a selected file.
  const file = await open(filePath, 'wx', 0o600)
  try {
    await file.writeFile(content, 'utf8')
    await file.sync()
  } finally {
    await file.close()
  }
}

export function registerProjectRegistrationExportIPC(): void {
  let busy = false
  ipcMain.handle(
    'db:project:exportRegistration',
    async (event): Promise<ProjectRegistrationExportResult> => {
      if (busy) return failure('project_registration_export_busy')
      busy = true
      try {
        const { content, count } = buildProjectRegistrationExport(
          getPublicDatabase(),
          process.platform === 'win32' ? 'windows' : 'posix'
        )
        const options: Electron.SaveDialogOptions = {
          defaultPath: 'uebox-project-registration.json',
          filters: [{ name: 'JSON', extensions: ['json'] }],
          properties: ['dontAddToRecent']
        }
        const parent = BrowserWindow.fromWebContents(event.sender)
        const selected = parent
          ? await dialog.showSaveDialog(parent, options)
          : await dialog.showSaveDialog(options)
        if (selected.canceled || !selected.filePath || event.sender.isDestroyed()) {
          return failure('project_registration_export_cancelled')
        }
        if (!isAbsolute(selected.filePath) || selected.filePath.includes('\0')) {
          return failure('project_registration_export_invalid')
        }
        try {
          await writeExclusive(selected.filePath, content)
        } catch (error) {
          const exists = error instanceof Error && 'code' in error && error.code === 'EEXIST'
          return failure(
            exists
              ? 'project_registration_export_exists'
              : 'project_registration_export_write_failed'
          )
        }
        return { success: true, code: 'project_registration_exported', count }
      } catch (error) {
        return failure(
          error instanceof InvalidProjectRegistrationError
            ? 'project_registration_export_invalid'
            : 'project_registration_export_unavailable'
        )
      } finally {
        busy = false
      }
    }
  )
}
