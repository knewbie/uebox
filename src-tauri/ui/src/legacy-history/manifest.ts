import { MAX_MANIFEST_BYTES } from './commands'

export class ManifestReadError extends Error {
  constructor(readonly code: 'tooLarge' | 'invalidUtf8' | 'readFailed') {
    super(code)
  }
}

export async function readLegacyManifest(file: File): Promise<string> {
  if (file.size > MAX_MANIFEST_BYTES) throw new ManifestReadError('tooLarge')
  let bytes: ArrayBuffer
  try {
    bytes = await file.arrayBuffer()
  } catch {
    throw new ManifestReadError('readFailed')
  }
  if (bytes.byteLength > MAX_MANIFEST_BYTES) throw new ManifestReadError('tooLarge')
  try {
    return new TextDecoder('utf-8', { fatal: true }).decode(bytes)
  } catch {
    throw new ManifestReadError('invalidUtf8')
  }
}
