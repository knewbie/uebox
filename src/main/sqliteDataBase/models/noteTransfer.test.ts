import Database from 'better-sqlite3'
import { createHash } from 'node:crypto'
import { afterEach, beforeEach, expect, it } from 'vitest'
import { initNoteModel } from './note'
import { initNotebookModel } from './notebook'
import { buildNoteTransfer } from './noteTransfer'

let db: Database.Database

beforeEach(() => {
  db = new Database(':memory:')
  initNoteModel(db)
  initNotebookModel(db)
})

afterEach(() => db.close())

it('exports visible text, inline binaries, and original-file sources with exact legacy IDs', () => {
  const png = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])
  const dataUri = `data:image/png;base64,${png.toString('base64')}`
  db.exec(`
    INSERT INTO notebooks (notebook_id, title) VALUES ('book-a', 'Notebook A');
    INSERT INTO notes (id, title, content, tags)
      VALUES (9007199254740993, 'Standalone',
        '<p>First &amp; second</p><p>Third<img src="${dataUri}"></p><script>secret()</script>',
        'private-tag');
    INSERT INTO notebook_sources (source_id, notebook_id, title, type, content, source_url, file_name)
      VALUES
        ('a', 'book-a', 'HTML note', 'note', '<p>你好<br>世界</p>', NULL, NULL),
        ('b', 'book-a', 'Plain text', 'text', '# Literal <b>tag</b>', NULL, NULL),
        ('c', 'book-a', 'Report', 'file', 'private linked text', 'file:///private/document', 'report.pdf');
  `)

  const exported = buildNoteTransfer(db)
  expect(exported.count).toBe(4)
  expect(exported.skipped).toBe(0)
  const parsed = JSON.parse(exported.content)
  expect(parsed.format).toBe('uebox-notes')
  expect(parsed.version).toBe(2)
  expect(parsed.source).toBe('electron-public-notes')
  expect(parsed.notebooks).toEqual([{ identity: 'book-a', title: 'Notebook A' }])
  expect(parsed.notes).toEqual([
    {
      identity: 'note:9007199254740993',
      notebookIdentity: null,
      title: 'Standalone',
      content: 'First & second\nThird',
      format: 'html',
      attachments: [
        {
          kind: 'binary',
          mediaType: 'image/png',
          fileName: null,
          bytes: png.toString('base64'),
          sha256: createHash('sha256').update(png).digest('hex')
        }
      ]
    },
    {
      identity: 'source:a',
      notebookIdentity: 'book-a',
      title: 'HTML note',
      content: '你好\n世界',
      format: 'html',
      attachments: []
    },
    {
      identity: 'source:b',
      notebookIdentity: 'book-a',
      title: 'Plain text',
      content: '# Literal <b>tag</b>',
      format: 'plain',
      attachments: []
    }
  ])
  expect(parsed.originalFiles).toEqual([
    {
      identity: 'c',
      notebookIdentity: 'book-a',
      kind: 'file',
      title: 'Report',
      content: 'private linked text',
      rawContent: null,
      sourceUrl: 'file:///private/document',
      fileName: 'report.pdf',
      filePath: null,
      mediaUrl: null
    }
  ])
  // Notes stay safe: no script content and no private tags leak.
  expect(parsed.notes.map((note) => note.content).join('\n')).not.toMatch(/secret\(|private-tag/)
  // Original-file provenance is exported as data; the local file is never read.
  expect(db.prepare('SELECT count(*) AS count FROM notebook_sources').get()).toEqual({ count: 3 })
})

it('never extracts remote or local image references', () => {
  db.exec(`
    INSERT INTO notes (title, content) VALUES ('Mixed', '<p>
      <img src="https://example.com/remote.png">
      <img src="file:///tmp/local.png">
      <img src="data:image/png;base64,iVBORw0KGgo=">
    </p>');
  `)
  const parsed = JSON.parse(buildNoteTransfer(db).content)
  expect(parsed.notes[0].attachments).toEqual([
    {
      kind: 'binary',
      mediaType: 'image/png',
      fileName: null,
      bytes: 'iVBORw0KGgo=',
      sha256: createHash('sha256').update(Buffer.from('iVBORw0KGgo=', 'base64')).digest('hex')
    }
  ])
})

it('bounds encoded archive size and excludes oversized raw rows', () => {
  const insert = db.prepare('INSERT INTO notes(title, content) VALUES (?, ?)')
  insert.run('Oversized', 'x'.repeat(4 * 1024 * 1024 + 1))
  expect(buildNoteTransfer(db)).toMatchObject({ count: 0, skipped: 1 })
  for (let index = 0; index < 4; index++) {
    insert.run('Fixture', 'x'.repeat(4 * 1024 * 1024))
  }
  expect(() => buildNoteTransfer(db)).toThrow('byte_limit')
})

it('bounds original-file sources at 16384 rows, counting invalid rows too', () => {
  db.exec(`INSERT INTO notebooks (notebook_id, title) VALUES ('book-a', 'Notebook A')`)
  const insert = db.prepare(
    "INSERT INTO notebook_sources (source_id, notebook_id, title, type, content) VALUES (?, 'book-a', 'S', 'file', 'x')"
  )
  const fill = db.transaction((count: number) => {
    for (let index = 0; index < count; index++) insert.run(`src-${index}`)
  })
  fill(16384)
  expect(buildNoteTransfer(db).count).toBe(16384)

  // The 16,385th row is invalid (oversized title), but it still counts
  // toward the scan bound and fails the export closed.
  db.prepare(
    "INSERT INTO notebook_sources (source_id, notebook_id, title, type, content) VALUES ('src-invalid', 'book-a', ?, 'file', 'x')"
  ).run('x'.repeat(1025))
  expect(() => buildNoteTransfer(db)).toThrow('original_limit')
})
