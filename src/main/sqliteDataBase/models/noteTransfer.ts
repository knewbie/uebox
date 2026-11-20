import type Database from 'better-sqlite3'
import { Buffer } from 'node:buffer'
import { load } from 'cheerio'

const MAX_DOCUMENT_BYTES = 16 * 1024 * 1024
const MAX_NOTE_BYTES = 4 * 1024 * 1024
const MAX_TITLE_BYTES = 1024
const MAX_NOTEBOOKS = 4096
const MAX_NOTES = 16_384

interface NotebookRow {
  identity: unknown
  title: unknown
}

interface NoteRow extends NotebookRow {
  notebookIdentity: unknown
  content: unknown
  richText: 0 | 1
}

interface PortableNote {
  identity: string
  notebookIdentity: string | null
  title: string
  content: string
}

export interface NoteTransferResult {
  content: string
  count: number
  skipped: number
}

export class InvalidNoteTransferError extends Error {
  constructor(readonly code: 'invalid_source' | 'notebook_limit' | 'note_limit' | 'byte_limit') {
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

function projectNote(row: NoteRow, notebookIds: ReadonlySet<string>): PortableNote | null {
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

  const html = row.richText && /<\s*(?:[a-z][^>]*>|!--)/i.test(row.content)
  const content = html ? visibleRichText(row.content) : row.content
  if (!validText(content, MAX_NOTE_BYTES)) return null
  return {
    identity: notebookIdentity === null ? `note:${identity}` : `source:${identity}`,
    notebookIdentity,
    title,
    content
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
  const document = { format: 'uebox-notes', version: 1, source: 'electron-public-notes', notebooks }
  const prefix = JSON.stringify(document).slice(0, -1) + ',"notes":['
  const suffix = ']}\n'
  let bytes = Buffer.byteLength(prefix) + Buffer.byteLength(suffix)
  let skipped = 0
  let scanned = 0
  const notes: string[] = []

  const append = (rows: Iterable<NoteRow>): void => {
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

  const fields = `CASE WHEN length(CAST(title AS BLOB)) <= ${MAX_TITLE_BYTES} THEN title END AS title,
    CASE WHEN length(CAST(content AS BLOB)) <= ${MAX_NOTE_BYTES} THEN content END AS content`
  const independent = db.prepare<[number], NoteRow>(
    `SELECT CAST(id AS TEXT) AS identity, NULL AS notebookIdentity, 1 AS richText, ${fields}
     FROM main.notes ORDER BY id LIMIT ?`
  )
  append(independent.iterate(MAX_NOTES + 1))

  if (sources) {
    const linked = db.prepare<[number], NoteRow>(
      `SELECT CAST(source_id AS TEXT) AS identity, CAST(notebook_id AS TEXT) AS notebookIdentity,
              (type = 'note') AS richText, ${fields}
       FROM main.notebook_sources WHERE type IN ('note', 'text') ORDER BY source_id LIMIT ?`
    )
    append(linked.iterate(MAX_NOTES - scanned + 1))
    const unsupported = db
      .prepare<[], { count: number }>(
        `SELECT count(*) AS count FROM main.notebook_sources
       WHERE type IS NULL OR type NOT IN ('note', 'text')`
      )
      .get()
    skipped += unsupported?.count ?? 0
  }

  return { content: prefix + notes.join(',') + suffix, count: notes.length, skipped }
}

/** Export one read-only snapshot of the public notebook and note tables. */
export function buildNoteTransfer(db: Database.Database): NoteTransferResult {
  try {
    return db.transaction(() => readSource(db))()
  } catch (error) {
    if (error instanceof InvalidNoteTransferError) throw error
    throw new InvalidNoteTransferError('invalid_source')
  }
}
