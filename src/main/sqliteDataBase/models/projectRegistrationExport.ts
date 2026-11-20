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

/**
 * One v2 registration record. Field meanings are fixed so the Rust importer can
 * align them directly with `ProjectMetadata` / `set_metadata(project_id, note,
 * description, is_pinned, sort_order)` in `crates/uebox-project-library`:
 *
 * - `note`      → target `note` (source `projects.note`; `null` when absent,
 *                 matching `normalize_text` which treats empty text as `None`).
 * - `pinned`    → target `is_pinned` (`projects.isPinned` 0/1 as a boolean).
 * - `sortOrder` → target `sort_order` (`projects.sort_order`).
 *
 * `description` is intentionally absent: the Electron `projects` table has no
 * description column, so this export must not invent one or fill it with
 * `note`. The importer leaves the target `description` at its default.
 */
export interface ProjectRegistrationV2Project {
  engine: 'unreal'
  name: string
  path: string
  note: string | null
  pinned: boolean
  sortOrder: number
}

export interface ProjectRegistrationV2Document {
  format: 'uebox-project-registration'
  version: 2
  pathFamily: ProjectRegistrationPathFamily
  projects: ProjectRegistrationV2Project[]
}

/** Matches the target `project_metadata.note` CHECK: 1..=1048576 bytes, no NUL. */
const MAX_NOTE_BYTES = 1_048_576

const projectRowV2 = z.object({
  projectName: z
    .string()
    .refine((name) => !name.includes('\0'))
    .nullish(),
  projectPath: z.string().nullish(),
  originPath: z.string().nullish(),
  note: z
    .string()
    .refine((note) => !note.includes('\0') && Buffer.byteLength(note, 'utf8') <= MAX_NOTE_BYTES)
    .nullish(),
  isPinned: z.union([z.literal(0), z.literal(1)]).nullish(),
  sortOrder: z.number().finite().nullish()
})

/**
 * Select the v2 metadata columns, defaulting columns that predate the current
 * schema (a legacy database may lack `isPinned` or `sort_order`).
 */
function projectMetadataSelect(db: Database.Database): string {
  const columns = new Set(
    (db.pragma('table_info(projects)') as Array<{ name: string }>).map((column) => column.name)
  )
  return [
    'projectName',
    'projectPath',
    'originPath',
    columns.has('note') ? 'note' : 'CAST(NULL AS TEXT) AS note',
    columns.has('isPinned') ? 'isPinned' : '0 AS isPinned',
    columns.has('sort_order') ? 'sort_order AS sortOrder' : '0.0 AS sortOrder'
  ].join(', ')
}

/**
 * v2 adds text-only project metadata (`note`, `pinned`, `sortOrder`) on top of
 * the v1 registration fields. `description` is not exported because the source
 * table has no such column. Cover and resource-source paths are never read or
 * exported. The v1 `buildProjectRegistrationExport` entry is unchanged.
 */
export function buildProjectRegistrationExportV2(
  db: Database.Database,
  pathFamily: ProjectRegistrationPathFamily
): { content: string; count: number } {
  const rows = db
    .prepare(`SELECT ${projectMetadataSelect(db)} FROM projects ORDER BY id ASC LIMIT ?`)
    .all(MAX_EXPORT_PROJECTS + 1)
  if (rows.length > MAX_EXPORT_PROJECTS) throw new InvalidProjectRegistrationError()
  const document: ProjectRegistrationV2Document = {
    format: 'uebox-project-registration',
    version: 2,
    pathFamily,
    projects: rows.map((row) => {
      const parsed = projectRowV2.safeParse(row)
      if (!parsed.success) throw new InvalidProjectRegistrationError()
      const directory = projectDirectory(parsed.data, pathFamily)
      return {
        engine: 'unreal',
        name: projectName(parsed.data, directory, pathFamily),
        path: directory,
        note: parsed.data.note ? parsed.data.note : null,
        pinned: parsed.data.isPinned === 1,
        sortOrder: parsed.data.sortOrder ?? 0
      }
    })
  }
  const content = `${JSON.stringify(document)}\n`
  if (Buffer.byteLength(content, 'utf8') > MAX_EXPORT_BYTES) {
    throw new InvalidProjectRegistrationError()
  }
  return { content, count: document.projects.length }
}
