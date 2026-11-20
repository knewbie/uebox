import { BrowserWindow, app, dialog, ipcMain } from 'electron'
import { constants } from 'node:fs'
import { lstat, open } from 'node:fs/promises'
import { basename, dirname, extname, isAbsolute, join } from 'node:path'
import type { PortableModelExportReply } from '../../shared/modelProfileTransfer'
import { exportModelProfiles } from './modelProfileExport'

const SOURCE_LIMIT = 2 * 1024 * 1024

async function readCatalog(source: string): Promise<string> {
  const metadata = await lstat(source)
  if (!metadata.isFile() || metadata.nlink !== 1 || metadata.size > SOURCE_LIMIT) {
    throw new Error('model_profile_export_invalid')
  }
  const flags =
    process.platform === 'win32'
      ? constants.O_RDONLY
      : constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK
  const file = await open(source, flags)
  try {
    const actual = await file.stat()
    if (
      !actual.isFile() ||
      actual.dev !== metadata.dev ||
      actual.ino !== metadata.ino ||
      actual.size !== metadata.size
    ) {
      throw new Error('model_profile_export_invalid')
    }
    const data = Buffer.alloc(actual.size + 1)
    let received = 0
    while (received < data.length) {
      const { bytesRead } = await file.read(data, received, data.length - received, null)
      if (!bytesRead) break
      received += bytesRead
    }
    if (received !== actual.size || (await file.stat()).mtimeMs !== actual.mtimeMs) {
      throw new Error('model_profile_export_invalid')
    }
    return new TextDecoder('utf-8', { fatal: true }).decode(data.subarray(0, received))
  } finally {
    await file.close()
  }
}

export function registerModelProfileExportIPC(): void {
  let busy = false
  ipcMain.handle(
    'ai-provider:export-model-profiles',
    async (event): Promise<PortableModelExportReply> => {
      if (busy) return { success: false, code: 'model_profile_export_busy' }
      busy = true
      try {
        const options: Electron.SaveDialogOptions = {
          defaultPath: 'uebox-model-profiles.json',
          filters: [{ name: 'JSON', extensions: ['json'] }],
          properties: ['dontAddToRecent']
        }
        const parent = BrowserWindow.fromWebContents(event.sender)
        const selected = parent
          ? await dialog.showSaveDialog(parent, options)
          : await dialog.showSaveDialog(options)
        if (selected.canceled || !selected.filePath || event.sender.isDestroyed()) {
          return { success: false, code: 'model_profile_export_cancelled' }
        }
        const source = join(app.getPath('userData'), 'models.json')
        const destination = selected.filePath
        if (
          !isAbsolute(destination) ||
          extname(destination).toLowerCase() !== '.json' ||
          (dirname(destination) === dirname(source) &&
            basename(destination).toLowerCase() === 'models.json')
        ) {
          return { success: false, code: 'model_profile_export_invalid' }
        }
        const converted = exportModelProfiles(await readCatalog(source))
        if (!converted.success) return converted
        const handle = await open(destination, 'wx', 0o600)
        try {
          await handle.writeFile(JSON.stringify(converted.config), 'utf8')
          await handle.sync()
        } finally {
          await handle.close()
        }
        return {
          success: true,
          profiles: converted.config.profiles.length,
          unsupportedModels: converted.counts.unsupported.models
        }
      } catch (cause) {
        if (cause instanceof Error && 'code' in cause && cause.code === 'EEXIST') {
          return { success: false, code: 'model_profile_export_exists' }
        }
        return { success: false, code: 'model_profile_export_unavailable' }
      } finally {
        busy = false
      }
    }
  )
}
