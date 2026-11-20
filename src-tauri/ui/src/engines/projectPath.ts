export function isAbsoluteProjectPath(path: string, os: string | null): boolean {
  if (os === null || path.includes('\0')) return false
  if (os === 'windows') {
    const normalized = path.replace(/\\/g, '/')
    return (
      /^(?:[a-zA-Z]:\/|\/\/[^/]+\/[^/]+)/.test(normalized) && !normalized.split('/').includes('..')
    )
  }
  return path.startsWith('/') && !path.split('/').includes('..')
}
