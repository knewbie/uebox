import type Database from 'better-sqlite3'
import { Buffer } from 'node:buffer'
import { posix, win32 } from 'node:path'
import { z } from 'zod'

export type ProjectRegistrationPathFamily = 'windows' | 'posix'

export interface ProjectRegistrationDocument {
  format: 'uebox-project-registration'
  version: 1
  pathFamily: ProjectRegistrationPathFamily
  projects: Array<{ engine: 'unreal'; name: string; path: string }>
}

const MAX_EXPORT_BYTES = 1_048_576
const MAX_EXPORT_PROJECTS = 4096
const projectRow = z.object({
  projectName: z
    .string()
    .refine((name) => !name.includes('\0'))
    .nullish(),
  projectPath: z.string().nullish(),
  originPath: z.string().nullish()
})

export class InvalidProjectRegistrationError extends Error {
  constructor() {
    super('Invalid project registration export')
    this.name = 'InvalidProjectRegistrationError'
  }
}

function absoluteDirectory(value: string, family: ProjectRegistrationPathFamily): boolean {
  if (value.includes('\0')) return false
  if (family === 'posix') return posix.isAbsolute(value)
  // win32.isAbsolute alone accepts drive-relative roots such as "\\Game".
  // Device namespaces are not portable project registrations.
  if (/^[\\/]{2}[?.](?:[\\/]|$)/.test(value)) return false
  return /^[a-z]:[\\/]/i.test(value) || /^[\\/]{2}[^\\/]+[\\/][^\\/]+(?:[\\/]|$)/.test(value)
}

function projectDirectory(
  row: z.infer<typeof projectRow>,
  family: ProjectRegistrationPathFamily
): string {
  if (row.projectPath?.trim()) {
    if (absoluteDirectory(row.projectPath, family)) return row.projectPath
    throw new InvalidProjectRegistrationError()
  }
  const origin = row.originPath
  if (!origin || !/\.uproject$/i.test(origin) || !absoluteDirectory(origin, family)) {
    throw new InvalidProjectRegistrationError()
  }
  return (family === 'windows' ? win32 : posix).dirname(origin)
}

function projectName(
  row: z.infer<typeof projectRow>,
  directory: string,
  family: ProjectRegistrationPathFamily
): string {
  if (row.projectName?.trim()) return row.projectName
  const paths = family === 'windows' ? win32 : posix
  const normalized = paths.normalize(directory)
  const name = normalized === paths.parse(normalized).root ? '' : paths.basename(normalized)
  if (name.trim()) return name
  const origin = row.originPath
  if (origin && /\.uproject$/i.test(origin) && absoluteDirectory(origin, family)) {
    const stem = paths.basename(origin).slice(0, -'.uproject'.length)
    if (stem.trim()) return stem
  }
  throw new InvalidProjectRegistrationError()
}

/** One bounded SELECT on the already-open connection; never inspect or normalize through the FS. */
export function buildProjectRegistrationExport(
  db: Database.Database,
  pathFamily: ProjectRegistrationPathFamily
): { content: string; count: number } {
  const rows = db
    .prepare('SELECT projectName, projectPath, originPath FROM projects ORDER BY id ASC LIMIT ?')
    .all(MAX_EXPORT_PROJECTS + 1)
  if (rows.length > MAX_EXPORT_PROJECTS) throw new InvalidProjectRegistrationError()
  const document: ProjectRegistrationDocument = {
    format: 'uebox-project-registration',
    version: 1,
    pathFamily,
    projects: rows.map((row) => {
      const parsed = projectRow.safeParse(row)
      if (!parsed.success) throw new InvalidProjectRegistrationError()
      const directory = projectDirectory(parsed.data, pathFamily)
      return {
        engine: 'unreal',
        name: projectName(parsed.data, directory, pathFamily),
        path: directory
      }
    })
  }
  const content = `${JSON.stringify(document)}\n`
  if (Buffer.byteLength(content, 'utf8') > MAX_EXPORT_BYTES) {
    throw new InvalidProjectRegistrationError()
  }
  return { content, count: document.projects.length }
}
