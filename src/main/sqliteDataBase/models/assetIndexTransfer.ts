import type Database from 'better-sqlite3'
import { Buffer } from 'node:buffer'

const MAX_BYTES = 16 * 1024 * 1024
const MAX_FOLDERS = 8192
const MAX_ASSETS = 32_768
const MAX_ID_BYTES = 256
const MAX_NAME_BYTES = 1024
const MAX_PATH_BYTES = 8192
const MAX_METADATA_BYTES = 64
const MAX_TAGS = 64
const MAX_TAG_BYTES = 256
const CONTROL = /\p{Cc}/u

export interface AssetIndexVaultRegistration {
  readonly id: string
  readonly name: string
  readonly path: string
}

export interface AssetIndexTransferDocument {
  format: 'uebox-local-asset-index'
  version: 1
  source: 'electron-local-vault'
  vault: { identity: string; name: string; rootPath: string; indexAvailable: boolean }
  folders: Array<{ identity: string; parentIdentity: string | null; name: string }>
  assets: Array<{
    identity: string
    folderIdentity: string | null
    name: string
    relativePath: string | null
    size: number | null
    extension: string | null
    modifiedTime: string | null
    tags: string[]
  }>
}

export interface AssetIndexTransferResult {
  content: string
  folderCount: number
  assetCount: number
}

export class InvalidAssetIndexTransferError extends Error {
  constructor(readonly code: 'invalid_source' | 'folder_limit' | 'asset_limit' | 'byte_limit') {
    super(`Invalid asset index transfer (${code})`)
    this.name = 'InvalidAssetIndexTransferError'
  }
}

interface LexicalPath {
  windows: boolean
  anchor: string
  segments: string[]
}

type Row = Record<string, unknown>
type Table = 'assetFolder' | 'assetData'

function validText(value: unknown, limit: number): value is string {
  return (
    typeof value === 'string' &&
    !CONTROL.test(value) &&
    !/\p{Cs}/u.test(value) &&
    Buffer.byteLength(value) <= limit
  )
}

function requiredText(value: unknown, limit: number, identity = false): string {
  if (!validText(value, limit) || (identity && !value.length)) {
    throw new InvalidAssetIndexTransferError('invalid_source')
  }
  return value
}

function optionalText(value: unknown): string | null {
  return validText(value, MAX_METADATA_BYTES) && value.length ? value : null
}

function segments(value: string, windows: boolean): string[] | null {
  if (
    CONTROL.test(value) ||
    value.includes(':') ||
    /\p{Cs}/u.test(value) ||
    (!windows && value.includes('\\'))
  )
    return null
  const parts = value.split(windows ? /[\\/]/ : '/').filter((part) => part && part !== '.')
  if (parts.some((part) => part === '..')) return null
  if (
    windows &&
    parts.some(
      (part) =>
        /[<>:"|?*]/.test(part) ||
        /[. ]$/.test(part) ||
        /^(?:con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)/i.test(part)
    )
  )
    return null
  return parts
}

/** Absolute grammar is independent of the OS running Electron; no cwd or filesystem lookup. */
function absolutePath(value: string): LexicalPath | null {
  if (/^[\\/]{2}[?.](?:[\\/]|$)/.test(value)) return null
  const drive = /^([a-z]):[\\/](.*)$/is.exec(value)
  if (drive) {
    const parts = segments(drive[2], true)
    return parts ? { windows: true, anchor: drive[1].toLowerCase(), segments: parts } : null
  }
  const unc = /^[\\/]{2}([^\\/]+)[\\/]([^\\/]+)(?:[\\/](.*))?$/s.exec(value)
  if (unc) {
    if ([unc[1], unc[2]].some((part) => part === '.' || part === '..')) return null
    const parts = segments(`${unc[1]}/${unc[2]}/${unc[3] ?? ''}`, true)
    return parts
      ? {
          windows: true,
          anchor: `//${unc[1].toLowerCase()}/${unc[2].toLowerCase()}`,
          segments: parts.slice(2)
        }
      : null
  }
  if (value.startsWith('/') && !value.startsWith('//')) {
    const parts = segments(value.slice(1), false)
    return parts ? { windows: false, anchor: '/', segments: parts } : null
  }
  return null
}

function relativePath(root: LexicalPath, value: unknown): string | null {
  if (typeof value !== 'string' || !value.length) return null
  const absolute = absolutePath(value)
  if (!absolute) {
    if (/^[\\/]|^[a-z][a-z\d+.-]*:/i.test(value)) return null
    const parts = segments(value, root.windows)
    const relative = parts?.join('/')
    return relative && validText(relative, MAX_PATH_BYTES) ? relative : null
  }
  if (absolute.windows !== root.windows || absolute.anchor !== root.anchor) return null
  const comparable = (part: string): string => (root.windows ? part.toLowerCase() : part)
  if (
    !root.segments.every(
      (part, index) => comparable(part) === comparable(absolute.segments[index] ?? '')
    )
  )
    return null
  const parts = absolute.segments.slice(root.segments.length)
  const relative = parts.join('/')
  return relative && validText(relative, MAX_PATH_BYTES) ? relative : null
}

function columns(db: Database.Database, table: Table, required: readonly string[]): Set<string> {
  const schema = db
    .prepare<
      [string],
      { type: string; sql: string }
    >('SELECT type, sql FROM main.sqlite_schema WHERE name = ? COLLATE NOCASE')
    .get(table)
  if (schema?.type !== 'table' || !/^CREATE\s+TABLE\b/i.test(schema.sql)) {
    throw new InvalidAssetIndexTransferError('invalid_source')
  }
  const rows: Array<{ name: string }> = db
    .prepare<[], { name: string }>(`PRAGMA main.table_info("${table}")`)
    .all()
  const found = new Set<string>(rows.map(({ name }) => name.toLowerCase()))
  if (required.some((name) => !found.has(name.toLowerCase()))) {
    throw new InvalidAssetIndexTransferError('invalid_source')
  }
  return found
}

/** Only named columns cross into JS; oversized TEXT/BLOB values stay inside SQLite. */
function query(
  db: Database.Database,
  table: Table,
  found: ReadonlySet<string>,
  fields: Readonly<Record<string, string>>,
  numeric: Readonly<Record<string, string>> = {}
): Database.Statement<[number], Row> {
  const present = (name: string): boolean => found.has(name.toLowerCase())
  const text = Object.entries(fields).map(([alias, name]) =>
    present(name)
      ? `CASE WHEN length(CAST("${name}" AS BLOB)) <= ${MAX_BYTES} THEN "${name}" END AS "${alias}"`
      : `NULL AS "${alias}"`
  )
  const numbers = Object.entries(numeric).map(([alias, name]) =>
    present(name)
      ? `CASE WHEN typeof("${name}") IN ('integer', 'real') THEN "${name}" END AS "${alias}"`
      : `NULL AS "${alias}"`
  )
  const overflow = Object.values(fields)
    .filter(present)
    .map((name) => `length(CAST("${name}" AS BLOB)) > ${MAX_BYTES}`)
  const live = present('isDelete') ? 'COALESCE("isDelete", 0) = 0' : '1'
  return db.prepare<[number], Row>(
    `SELECT ${[...text, ...numbers].join(', ')}, (${overflow.join(' OR ') || '0'}) AS oversized
     FROM main."${table}" WHERE ${live}
     ORDER BY "${fields.identity}" COLLATE BINARY LIMIT ?`
  )
}

function checkRow(row: Row): void {
  if (row.oversized) throw new InvalidAssetIndexTransferError('byte_limit')
}

function validateForest(parents: ReadonlyMap<string, string | null>): void {
  const checked = new Set<string>()
  for (const identity of parents.keys()) {
    const chain = new Set<string>()
    let cursor: string | null = identity
    while (cursor !== null && !checked.has(cursor)) {
      if (chain.has(cursor)) throw new InvalidAssetIndexTransferError('invalid_source')
      chain.add(cursor)
      cursor = parents.get(cursor) ?? null
    }
    for (const key of chain) checked.add(key)
  }
}

function tags(value: unknown): string[] {
  if (value === null || value === '') return []
  if (typeof value !== 'string') throw new InvalidAssetIndexTransferError('invalid_source')
  const parsed: unknown = JSON.parse(value)
  if (!Array.isArray(parsed) || parsed.some((tag) => !validText(tag, MAX_TAG_BYTES))) {
    throw new InvalidAssetIndexTransferError('invalid_source')
  }
  const result = [...new Set(parsed as string[])]
  if (result.length > MAX_TAGS) throw new InvalidAssetIndexTransferError('invalid_source')
  return result
}

function size(value: unknown): number | null {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 ? value : null
}

function projectAsset(
  row: Row,
  ids: ReadonlySet<string>,
  root: LexicalPath
): AssetIndexTransferDocument['assets'][number] {
  // A present filePath is authoritative: an outside path must not be replaced by an old origin.
  const path = row.filePath === null || row.filePath === '' ? row.originPath : row.filePath
  return {
    identity: requiredText(row.identity, MAX_ID_BYTES, true),
    folderIdentity:
      typeof row.folderIdentity === 'string' && ids.has(row.folderIdentity)
        ? row.folderIdentity
        : null,
    name: requiredText(row.name, MAX_NAME_BYTES),
    relativePath: relativePath(root, path),
    size: size(row.fileSize) ?? size(row.size),
    extension: optionalText(row.extension) ?? optionalText(row.ext),
    modifiedTime: optionalText(row.modifiedTime),
    tags: tags(row.tags)
  }
}

type Append = (item: unknown, count: number) => void

function readFolderIds(db: Database.Database, found: ReadonlySet<string>): Set<string> {
  const ids = new Set<string>()
  let identityBytes = 0
  const rows = query(db, 'assetFolder', found, { identity: 'folderKey' })
  for (const row of rows.iterate(MAX_FOLDERS + 1)) {
    if (ids.size === MAX_FOLDERS) throw new InvalidAssetIndexTransferError('folder_limit')
    checkRow(row)
    const identity = requiredText(row.identity, MAX_ID_BYTES, true)
    if (ids.has(identity)) throw new InvalidAssetIndexTransferError('invalid_source')
    identityBytes += Buffer.byteLength(JSON.stringify(identity))
    if (identityBytes > MAX_BYTES) throw new InvalidAssetIndexTransferError('byte_limit')
    ids.add(identity)
  }
  return ids
}

function readFolders(db: Database.Database, append: Append): ReadonlySet<string> {
  const found = columns(db, 'assetFolder', ['folderKey', 'folderName'])
  const ids = readFolderIds(db, found)
  const parents = new Map<string, string | null>()
  const rows = query(db, 'assetFolder', found, {
    identity: 'folderKey',
    parentIdentity: 'fatherKey',
    name: 'folderName'
  })
  for (const row of rows.iterate(MAX_FOLDERS + 1)) {
    checkRow(row)
    const identity = requiredText(row.identity, MAX_ID_BYTES, true)
    const parentIdentity =
      typeof row.parentIdentity === 'string' && ids.has(row.parentIdentity)
        ? row.parentIdentity
        : null
    append({ identity, parentIdentity, name: requiredText(row.name, MAX_NAME_BYTES) }, parents.size)
    parents.set(identity, parentIdentity)
  }
  validateForest(parents)
  return ids
}

function readAssets(
  db: Database.Database,
  ids: ReadonlySet<string>,
  root: LexicalPath,
  append: Append
): number {
  const found = columns(db, 'assetData', ['assetKey', 'assetName', 'folderKey'])
  const rows = query(
    db,
    'assetData',
    found,
    {
      identity: 'assetKey',
      folderIdentity: 'folderKey',
      name: 'assetName',
      filePath: 'filePath',
      originPath: 'originPath',
      extension: 'fileExtension',
      ext: 'ext',
      modifiedTime: 'modifiedTime',
      tags: 'tags'
    },
    { fileSize: 'fileSize', size: 'size' }
  )
  const assetIds = new Set<string>()
  for (const row of rows.iterate(MAX_ASSETS + 1)) {
    if (assetIds.size === MAX_ASSETS) throw new InvalidAssetIndexTransferError('asset_limit')
    checkRow(row)
    const asset = projectAsset(row, ids, root)
    if (assetIds.has(asset.identity)) throw new InvalidAssetIndexTransferError('invalid_source')
    append(asset, assetIds.size)
    assetIds.add(asset.identity)
  }
  return assetIds.size
}

function readSource(
  vault: AssetIndexTransferDocument['vault'],
  root: LexicalPath,
  db: Database.Database | null
): AssetIndexTransferResult {
  const prefix =
    JSON.stringify({
      format: 'uebox-local-asset-index',
      version: 1,
      source: 'electron-local-vault',
      vault
    }).slice(0, -1) + ',"folders":['
  const middle = '],"assets":['
  const suffix = ']}\n'
  const parts = [prefix]
  let bytes = Buffer.byteLength(prefix + middle + suffix)
  if (bytes > MAX_BYTES) throw new InvalidAssetIndexTransferError('byte_limit')
  const append = (item: unknown, count: number): void => {
    const text = (count ? ',' : '') + JSON.stringify(item)
    bytes += Buffer.byteLength(text)
    if (bytes > MAX_BYTES) throw new InvalidAssetIndexTransferError('byte_limit')
    parts.push(text)
  }
  const ids = db ? readFolders(db, append) : new Set<string>()
  parts.push(middle)
  const assetCount = db ? readAssets(db, ids, root, append) : 0
  parts.push(suffix)
  return { content: parts.join(''), folderCount: ids.size, assetCount }
}

/**
 * One explicit manifest for one caller-selected local vault. Keys are scoped by vault.id.
 * Orphans become roots; deleted rows are omitted. Paths prove lexical containment only,
 * never file existence or symlink containment. No DB creation, filesystem IO, or credentials.
 * Only assetData.tags is available here; global numeric tag mappings need a separate source.
 */
export function buildAssetIndexTransfer(
  registration: AssetIndexVaultRegistration,
  db: Database.Database | null
): AssetIndexTransferResult {
  try {
    const vault = {
      identity: requiredText(registration.id, MAX_ID_BYTES, true),
      name: requiredText(registration.name, MAX_NAME_BYTES),
      rootPath: requiredText(registration.path, MAX_PATH_BYTES, true),
      indexAvailable: db !== null
    }
    const root = absolutePath(vault.rootPath)
    if (!root) throw new InvalidAssetIndexTransferError('invalid_source')
    return db ? db.transaction(() => readSource(vault, root, db))() : readSource(vault, root, null)
  } catch (error) {
    if (error instanceof InvalidAssetIndexTransferError) throw error
    // SQLite diagnostics and malformed JSON can contain private paths or metadata.
    throw new InvalidAssetIndexTransferError('invalid_source')
  }
}
