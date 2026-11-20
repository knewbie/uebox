import type Database from 'better-sqlite3'
import { createHash } from 'node:crypto'
import { Buffer } from 'node:buffer'
import { load } from 'cheerio'

const MAX_DOCUMENT_BYTES = 16 * 1024 * 1024
const MAX_NOTE_BYTES = 4 * 1024 * 1024
const MAX_TITLE_BYTES = 1024
const MAX_NOTEBOOKS = 4096
const MAX_NOTES = 16_384
const MAX_ORIGINAL_FILES = 16_384
const MAX_ATTACHMENT_BYTES = 16 * 1024 * 1024
const MAX_ATTACHMENTS_PER_NOTE = 256

interface NotebookRow {
  identity: unknown
  title: unknown
}

interface NoteRow extends NotebookRow {
  notebookIdentity: unknown
  content: unknown
  richText: 0 | 1
}

interface OriginalFileRow {
  identity: unknown
  notebookIdentity: unknown
  kind: unknown
  title: unknown
  content: unknown
  rawContent: unknown
  sourceUrl: unknown
  fileName: unknown
  filePath: unknown
  mediaUrl: unknown
}

interface V2Attachment {
  kind: 'binary'
  mediaType: string | null
  fileName: string | null
  bytes: string
  sha256: string
}

interface V2Note {
  identity: string
  notebookIdentity: string | null
  title: string
  content: string
  format: 'html' | 'plain'
  attachments: V2Attachment[]
}

interface V2OriginalFile {
  identity: string
  notebookIdentity: string
  kind: string
  title: string
  content: string
  rawContent: string | null
  sourceUrl: string | null
  fileName: string | null
  filePath: string | null
  mediaUrl: string | null
}

export interface NoteTransferResult {
  content: string
  count: number
  skipped: number
}

export class InvalidNoteTransferError extends Error {
  constructor(
    readonly code: 'invalid_source' | 'notebook_limit' | 'note_limit' | 'original_limit' | 'byte_limit'
  ) {
    super(`Invalid note transfer (${code})`)
    this.name = 'InvalidNoteTransferError'
  }
}

function validText(value: unknown, maxBytes: number): value is string {
  return (
    typeof value === 'string' &&
    !value.includes('\0') &&
    Buffer.byteLength(value, 'utf8') <= maxBytes
  )
}

function optionalText(value: unknown, maxBytes: number): string | null {
  return validText(value, maxBytes) ? value : null
}

function visibleRichText(html: string): string {
  const document = load(html, {}, false)
  document(
    'script, style, iframe, object, embed, svg, math, template, noscript, form, input, video, audio, img, source, link, meta, base, [hidden], [aria-hidden="true"]'
  ).remove()
  document('br').replaceWith('\n')
  document('p, div, h1, h2, h3, h4, h5, h6, li, blockquote, pre, tr').each((_index, element) => {
    document(element).append('\n')
  })
  return document.root().text().trim()
}

/**
 * Decode a `data:` URI into its media type and bytes. Only inline image data is
 * accepted; no network or filesystem access is ever performed.
 */
function parseInlineImage(src: string): { mediaType: string; bytes: Buffer } | null {
  if (!src.startsWith('data:')) return null
  const comma = src.indexOf(',')
  if (comma === -1) return null
  const metadata = src.slice('data:'.length, comma)
  const data = src.slice(comma + 1)
  const isBase64 = metadata.endsWith(';base64')
  const mediaType = (isBase64 ? metadata.slice(0, -';base64'.length) : metadata).split(';')[0]
  if (!mediaType.startsWith('image/')) return null
  let bytes: Buffer
  try {
    bytes = isBase64 ? Buffer.from(data, 'base64') : Buffer.from(decodeURIComponent(data), 'utf8')
  } catch {
    return null
  }
  return { mediaType, bytes }
}

/** Extract inline `data:image/...` payloads from raw HTML into binary attachments. */
function extractInlineImages(html: string): V2Attachment[] {
  const document = load(html, {}, false)
  const attachments: V2Attachment[] = []
  let stopped = false
  document('img[src]').each((_index, element) => {
    if (stopped) return
    const src = document(element).attr('src')
    if (!src) return
    const parsed = parseInlineImage(src)
    if (!parsed || parsed.bytes.length === 0 || parsed.bytes.length > MAX_ATTACHMENT_BYTES) return
    const base64 = parsed.bytes.toString('base64')
    attachments.push({
      kind: 'binary',
      mediaType: parsed.mediaType,
      fileName: null,
      bytes: base64,
      sha256: createHash('sha256').update(parsed.bytes).digest('hex')
    })
    if (attachments.length >= MAX_ATTACHMENTS_PER_NOTE) stopped = true
  })
  return attachments
}

function projectNote(row: NoteRow, notebookIds: ReadonlySet<string>): V2Note | null {
  const { identity, notebookIdentity, title } = row
  if (
    !identity ||
    !validText(identity, 249) ||
    !validText(title, MAX_TITLE_BYTES) ||
    !validText(row.content, MAX_NOTE_BYTES)
  )
    return null
  if (
    notebookIdentity !== null &&
    (typeof notebookIdentity !== 'string' || !notebookIds.has(notebookIdentity))
  )
    return null

  const html = row.richText === 1 && /<\s*(?:[a-z][^>]*>|!--)/i.test(row.content)
  const content = html ? visibleRichText(row.content) : row.content
  if (!validText(content, MAX_NOTE_BYTES)) return null
  return {
    identity: notebookIdentity === null ? `note:${identity}` : `source:${identity}`,
    notebookIdentity,
    title,
    content,
    format: html ? 'html' : 'plain',
    attachments: html ? extractInlineImages(row.content) : []
  }
}

function projectOriginalFile(
  row: OriginalFileRow,
  notebookIds: ReadonlySet<string>
): V2OriginalFile | null {
  const identity = row.identity
  const notebookIdentity = row.notebookIdentity
  const title = row.title
  const content = row.content
  if (!identity || !validText(identity, 256)) return null
  if (notebookIdentity === null || typeof notebookIdentity !== 'string' || !notebookIds.has(notebookIdentity))
    return null
  if (!validText(title, MAX_TITLE_BYTES) || !validText(content, MAX_NOTE_BYTES)) return null
  return {
    identity,
    notebookIdentity,
    kind: typeof row.kind === 'string' ? row.kind : 'unknown',
    title,
    content,
    rawContent: optionalText(row.rawContent, MAX_NOTE_BYTES),
    sourceUrl: optionalText(row.sourceUrl, 4096),
    fileName: optionalText(row.fileName, 1024),
    filePath: optionalText(row.filePath, 4096),
    mediaUrl: optionalText(row.mediaUrl, 4096)
  }
}

function readSource(db: Database.Database): NoteTransferResult {
  const schema = db
    .prepare<[], { name: string; type: string }>(
      `SELECT name, type FROM main.sqlite_schema
     WHERE name COLLATE NOCASE IN ('notes', 'notebooks', 'notebook_sources')`
    )
    .all()
  const objects = new Map(schema.map(({ name, type }) => [name.toLowerCase(), type]))
  if (objects.get('notes') !== 'table' || objects.get('notebooks') !== 'table') {
    throw new InvalidNoteTransferError('invalid_source')
  }
  const sources = objects.get('notebook_sources')
  if (sources !== undefined && sources !== 'table') {
    throw new InvalidNoteTransferError('invalid_source')
  }

  const notebooks: Array<{ identity: string; title: string }> = db
    .prepare<[number], NotebookRow>(
      `SELECT CAST(notebook_id AS TEXT) AS identity,
            CASE WHEN length(CAST(title AS BLOB)) <= ${MAX_TITLE_BYTES} THEN title END AS title
     FROM main.notebooks ORDER BY notebook_id LIMIT ?`
    )
    .all(MAX_NOTEBOOKS + 1)
    .map(({ identity, title }) => {
      if (!identity || !validText(identity, 256) || !validText(title, MAX_TITLE_BYTES)) {
        throw new InvalidNoteTransferError('invalid_source')
      }
      return { identity, title }
    })
  if (notebooks.length > MAX_NOTEBOOKS) throw new InvalidNoteTransferError('notebook_limit')
  const notebookIds = new Set(notebooks.map(({ identity }) => identity))

  const document = {
    format: 'uebox-notes',
    version: 2,
    source: 'electron-public-notes',
    notebooks
  }
  const prefix = JSON.stringify(document).slice(0, -1) + ',"notes":['
  const middle = '],"originalFiles":['
  const suffix = ']}\n'
  let bytes = Buffer.byteLength(prefix) + Buffer.byteLength(middle) + Buffer.byteLength(suffix)
  let skipped = 0
  let scanned = 0
  let scannedOriginals = 0
  const notes: string[] = []
  const originalFiles: string[] = []

  const appendNotes = (rows: Iterable<NoteRow>): void => {
    for (const row of rows) {
      if (++scanned > MAX_NOTES) throw new InvalidNoteTransferError('note_limit')
      const note = projectNote(row, notebookIds)
      if (!note) {
        skipped++
        continue
      }
      const encoded = JSON.stringify(note)
      bytes += Buffer.byteLength(encoded) + (notes.length ? 1 : 0)
      if (bytes > MAX_DOCUMENT_BYTES) throw new InvalidNoteTransferError('byte_limit')
      notes.push(encoded)
    }
  }

  const appendOriginalFiles = (rows: Iterable<OriginalFileRow>): void => {
    for (const row of rows) {
      // The scan bound counts every source row, including invalid ones, so an
      // oversized table fails closed instead of silently truncating.
      if (++scannedOriginals > MAX_ORIGINAL_FILES) {
        throw new InvalidNoteTransferError('original_limit')
      }
      const file = projectOriginalFile(row, notebookIds)
      if (!file) {
        skipped++
        continue
      }
      const encoded = JSON.stringify(file)
      bytes += Buffer.byteLength(encoded) + (originalFiles.length ? 1 : 0)
      if (bytes > MAX_DOCUMENT_BYTES) throw new InvalidNoteTransferError('byte_limit')
      originalFiles.push(encoded)
    }
  }

  const noteFields = `CASE WHEN length(CAST(title AS BLOB)) <= ${MAX_TITLE_BYTES} THEN title END AS title,
    CASE WHEN length(CAST(content AS BLOB)) <= ${MAX_NOTE_BYTES} THEN content END AS content`
  const independent = db.prepare<[number], NoteRow>(
    `SELECT CAST(id AS TEXT) AS identity, NULL AS notebookIdentity, 1 AS richText, ${noteFields}
     FROM main.notes ORDER BY id LIMIT ?`
  )
  appendNotes(independent.iterate(MAX_NOTES + 1))

  if (sources) {
    const linked = db.prepare<[number], NoteRow>(
      `SELECT CAST(source_id AS TEXT) AS identity, CAST(notebook_id AS TEXT) AS notebookIdentity,
              (type = 'note') AS richText, ${noteFields}
       FROM main.notebook_sources WHERE type IN ('note', 'text') ORDER BY source_id LIMIT ?`
    )
    appendNotes(linked.iterate(MAX_NOTES - scanned + 1))

    const originals = db.prepare<[], OriginalFileRow>(
      `SELECT CAST(source_id AS TEXT) AS identity,
              CAST(notebook_id AS TEXT) AS notebookIdentity,
              type AS kind,
              CASE WHEN length(CAST(title AS BLOB)) <= ${MAX_TITLE_BYTES} THEN title END AS title,
              CASE WHEN length(CAST(content AS BLOB)) <= ${MAX_NOTE_BYTES} THEN content END AS content,
              CASE WHEN length(CAST(raw_content AS BLOB)) <= ${MAX_NOTE_BYTES} THEN raw_content END AS rawContent,
              CASE WHEN length(CAST(source_url AS BLOB)) <= 4096 THEN source_url END AS sourceUrl,
              CASE WHEN length(CAST(file_name AS BLOB)) <= 1024 THEN file_name END AS fileName,
              CASE WHEN length(CAST(file_path AS BLOB)) <= 4096 THEN file_path END AS filePath,
              CASE WHEN length(CAST(media_url AS BLOB)) <= 4096 THEN media_url END AS mediaUrl
       FROM main.notebook_sources
       WHERE type IS NULL OR type NOT IN ('note', 'text') ORDER BY source_id LIMIT ?`
    )
    appendOriginalFiles(originals.iterate(MAX_ORIGINAL_FILES + 1))
  }

  return {
    content: prefix + notes.join(',') + middle + originalFiles.join(',') + suffix,
    count: notes.length + originalFiles.length,
    skipped
  }
}

/** Export one read-only v2 snapshot of the public notebook, note, and source tables. */
export function buildNoteTransfer(db: Database.Database): NoteTransferResult {
  try {
    return db.transaction(() => readSource(db))()
  } catch (error) {
    if (error instanceof InvalidNoteTransferError) throw error
    throw new InvalidNoteTransferError('invalid_source')
  }
}
