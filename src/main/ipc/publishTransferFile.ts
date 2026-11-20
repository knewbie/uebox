import { link, mkdtemp, open, rm } from 'node:fs/promises'
import { dirname, join } from 'node:path'

export async function publishTransferFile(destination: string, content: string): Promise<void> {
  const staging = await mkdtemp(join(dirname(destination), '.uebox-transfer-'))
  const stagedFile = join(staging, 'export.json')
  try {
    const file = await open(stagedFile, 'wx', 0o600)
    try {
      await file.writeFile(content, 'utf8')
      await file.sync()
    } finally {
      await file.close()
    }
    await link(stagedFile, destination)
  } finally {
    await rm(staging, { recursive: true, force: true }).catch(() => undefined)
  }
}
