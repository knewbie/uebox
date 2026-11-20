import type Database from 'better-sqlite3'
import { Buffer } from 'node:buffer'
import { posix, win32 } from 'node:path'

import type { ProjectRegistrationPathFamily } from './projectRegistrationExport'

interface ProjectCollectionMember {
  engine: 'unreal'
  path: string
}

interface ProjectCollectionSnapshot {
  id: string
  name: string | null
  sortOrder: number
  members: ProjectCollectionMember[]
}

export interface ProjectCollectionDocument {
  format: 'uebox-project-collections'
  version: 1
  pathFamily: ProjectRegistrationPathFamily
  collections: ProjectCollectionSnapshot[]
}

type ExportErrorCode =
  | 'invalid_source'
  | 'invalid_collection'
  | 'invalid_project'
  | 'dangling_reference'
  | 'duplicate_id'
  | 'duplicate_membership'
  | 'collection_limit'
  | 'membership_limit'
  | 'byte_limit'

export class InvalidProjectCollectionExportError extends Error {
  constructor(readonly code: ExportErrorCode) {
    super(`Invalid project collection export (${code})`)
    this.name = 'InvalidProjectCollectionExportError'
  }
}

const MAX_COLLECTIONS = 4096
const MAX_MEMBERSHIPS = 16_384
const MAX_EXPORT_BYTES = 1_048_576
const MAX_ID_BYTES = 256
const MAX_NAME_BYTES = 1024
const MAX_REFERENCE_BYTES = 128 * 1024

interface CollectionRow {
  id: unknown
  name: unknown
  sortOrder: unknown
}

interface MemberRow {
  collectionId: unknown
  projectKey: unknown
  registered: number
  projectPath: unknown
  originPath: unknown
}

// Bound the bytes SQLite copies into JS, including fields sourced from damaged rows.
// An oversized value is surfaced as a non-text sentinel and fails validation.
function boundedText(column: string, maxBytes: number): string {
  return `CASE WHEN length(CAST(${column} AS BLOB)) > ${maxBytes} THEN 0 ELSE ${column} END`
}

function validCollectionId(value: unknown): value is string {
  return (
    typeof value === 'string' &&
    value.length > 0 &&
    value.trim() === value &&
    Buffer.byteLength(value, 'utf8') <= MAX_ID_BYTES &&
    !/\p{Cc}/u.test(value)
  )
}

function absoluteDirectory(value: string, family: ProjectRegistrationPathFamily): boolean {
  if (value.includes('\0')) return false
  if (family === 'posix') return posix.isAbsolute(value)
  // Match the registration export's path-family checks without inspecting the filesystem.
  if (/^[\\/]{2}[?.](?:[\\/]|$)/.test(value)) return false
  return /^[a-z]:[\\/]/i.test(value) || /^[\\/]{2}[^\\/]+[\\/][^\\/]+(?:[\\/]|$)/.test(value)
}

function originalDirectory(row: MemberRow, family: ProjectRegistrationPathFamily): string {
  const { projectPath, originPath } = row
  if (
    (projectPath !== null && typeof projectPath !== 'string') ||
    (originPath !== null && typeof originPath !== 'string')
  ) {
    throw new InvalidProjectCollectionExportError('invalid_project')
  }
  if (projectPath?.trim()) {
    if (absoluteDirectory(projectPath, family)) return projectPath
    throw new InvalidProjectCollectionExportError('invalid_project')
  }
  if (originPath && /\.uproject$/i.test(originPath) && absoluteDirectory(originPath, family)) {
    return (family === 'windows' ? win32 : posix).dirname(originPath)
  }
  throw new InvalidProjectCollectionExportError('invalid_project')
}

function hasMembershipTable(db: Database.Database): boolean {
  const rows = db
    .prepare<[], { name: string; type: string }>(
      `SELECT lower(name) AS name, type FROM main.sqlite_schema
       WHERE name COLLATE NOCASE IN ('project_collections', 'project_collection_items', 'projects')`
    )
    .all()
  const tables = new Map(rows.map(({ name, type }) => [name, type]))
  if (
    tables.get('project_collections') !== 'table' ||
    tables.get('projects') !== 'table' ||
    (tables.has('project_collection_items') && tables.get('project_collection_items') !== 'table')
  ) {
    throw new InvalidProjectCollectionExportError('invalid_source')
  }
  return tables.has('project_collection_items')
}

function memberRows(db: Database.Database, membershipTable: boolean): IterableIterator<MemberRow> {
  const projectColumns = new Set(
    (db.pragma('main.table_info(projects)') as Array<{ name: string }>).map(({ name }) => name)
  )
  if (
    !projectColumns.has('projectKey') ||
    (!projectColumns.has('projectPath') && !projectColumns.has('originPath')) ||
    (!membershipTable && !projectColumns.has('collectionKey'))
  ) {
    throw new InvalidProjectCollectionExportError('invalid_source')
  }

  const projectPath = projectColumns.has('projectPath')
    ? boundedText('p.projectPath', MAX_REFERENCE_BYTES)
    : 'NULL'
  const originPath = projectColumns.has('originPath')
    ? boundedText('p.originPath', MAX_REFERENCE_BYTES)
    : 'NULL'
  const collectionId = membershipTable ? 'm.collectionKey' : 'p.collectionKey'
  const projectKey = membershipTable ? 'm.projectKey' : 'p.projectKey'
  const source = membershipTable
    ? 'FROM main.project_collection_items AS m'
    : "FROM main.projects AS p WHERE p.collectionKey IS NOT NULL AND trim(p.collectionKey) <> ''"
  const { count } = db
    .prepare<[number], { count: number }>(
      `SELECT count(*) AS count FROM (SELECT 1 ${source} LIMIT ?)`
    )
    .safeIntegers(false)
    .get(MAX_MEMBERSHIPS + 1)!
  if (count > MAX_MEMBERSHIPS) throw new InvalidProjectCollectionExportError('membership_limit')

  // The relation table is authoritative when both layouts exist. Reading both
  // would reintroduce relationships a user already removed in the newer layout.
  const join = membershipTable ? 'LEFT JOIN main.projects AS p ON p.projectKey = m.projectKey' : ''
  return db
    .prepare<[number], MemberRow>(
      `SELECT ${boundedText(collectionId, MAX_ID_BYTES)} AS collectionId,
              ${boundedText(projectKey, MAX_REFERENCE_BYTES)} AS projectKey,
              p.projectKey IS NOT NULL AS registered,
              ${projectPath} AS projectPath, ${originPath} AS originPath
       ${source} ${join}
       ORDER BY ${collectionId} COLLATE BINARY, ${projectKey} COLLATE BINARY
       LIMIT ?`
    )
    .safeIntegers(false)
    .iterate(MAX_MEMBERSHIPS + 1)
}

function readSnapshot(
  db: Database.Database,
  pathFamily: ProjectRegistrationPathFamily
): { content: string; count: number } {
  const membershipTable = hasMembershipTable(db)
  const rows = db
    .prepare<[number], CollectionRow>(
      `SELECT ${boundedText('collectionKey', MAX_ID_BYTES)} AS id,
              ${boundedText('name', MAX_NAME_BYTES)} AS name, sort_order AS sortOrder
       FROM main.project_collections ORDER BY sort_order, collectionKey COLLATE BINARY LIMIT ?`
    )
    .safeIntegers(false)
    .all(MAX_COLLECTIONS + 1)
  if (rows.length > MAX_COLLECTIONS)
    throw new InvalidProjectCollectionExportError('collection_limit')

  const document: ProjectCollectionDocument = {
    format: 'uebox-project-collections',
    version: 1,
    pathFamily,
    collections: []
  }
  let bytes = Buffer.byteLength(`${JSON.stringify(document)}\n`, 'utf8')
  function account(value: unknown, separator: boolean): void {
    bytes += Buffer.byteLength(JSON.stringify(value), 'utf8') + Number(separator)
    if (bytes > MAX_EXPORT_BYTES) throw new InvalidProjectCollectionExportError('byte_limit')
  }

  const byId = new Map<string, { collection: ProjectCollectionSnapshot; paths: Set<string> }>()
  for (const { id, name, sortOrder } of rows) {
    if (
      !validCollectionId(id) ||
      (name !== null &&
        (typeof name !== 'string' ||
          name.includes('\0') ||
          Buffer.byteLength(name, 'utf8') > MAX_NAME_BYTES)) ||
      typeof sortOrder !== 'number' ||
      !Number.isSafeInteger(sortOrder)
    ) {
      throw new InvalidProjectCollectionExportError('invalid_collection')
    }
    if (byId.has(id)) throw new InvalidProjectCollectionExportError('duplicate_id')
    const collection = { id, name, sortOrder, members: [] }
    account(collection, document.collections.length > 0)
    byId.set(id, { collection, paths: new Set() })
    document.collections.push(collection)
  }

  let seen = 0
  let previousCollection: string | undefined
  let previousProject: string | undefined
  for (const row of memberRows(db, membershipTable)) {
    if (++seen > MAX_MEMBERSHIPS) throw new InvalidProjectCollectionExportError('membership_limit')
    const { collectionId, projectKey, registered } = row
    const entry = typeof collectionId === 'string' ? byId.get(collectionId) : undefined
    if (
      !entry ||
      !registered ||
      typeof projectKey !== 'string' ||
      !projectKey.trim() ||
      projectKey.includes('\0')
    ) {
      throw new InvalidProjectCollectionExportError('dangling_reference')
    }
    if (previousCollection === collectionId && previousProject === projectKey) {
      throw new InvalidProjectCollectionExportError('duplicate_membership')
    }
    previousCollection = entry.collection.id
    previousProject = projectKey
    const path = originalDirectory(row, pathFamily)
    if (entry.paths.has(path)) continue
    entry.paths.add(path)
    const member: ProjectCollectionMember = { engine: 'unreal', path }
    account(member, entry.collection.members.length > 0)
    entry.collection.members.push(member)
  }
  return { content: `${JSON.stringify(document)}\n`, count: document.collections.length }
}

/** Read a consistent snapshot on the caller's connection without initializing or migrating it. */
export function buildProjectCollectionExport(
  db: Database.Database,
  pathFamily: ProjectRegistrationPathFamily
): { content: string; count: number } {
  if (pathFamily !== 'posix' && pathFamily !== 'windows') {
    throw new InvalidProjectCollectionExportError('invalid_source')
  }
  try {
    return db.transaction(() => readSnapshot(db, pathFamily))()
  } catch (error) {
    if (error instanceof InvalidProjectCollectionExportError) throw error
    throw new InvalidProjectCollectionExportError('invalid_source')
  }
}
