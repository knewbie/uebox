import Database from 'better-sqlite3'
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

it('exports only visible local note content and retains exact legacy IDs', () => {
  db.exec(`
    INSERT INTO notebooks (notebook_id, title) VALUES ('book-a', 'Notebook A');
    INSERT INTO notes (id, title, content, tags)
      VALUES (9007199254740993, 'Standalone',
        '<p>First &amp; second</p><p>Third<img src="file:///private/photo.png"></p><script>secret()</script>',
        'private-tag');
    INSERT INTO notebook_sources (source_id, notebook_id, title, type, content, source_url)
      VALUES
        ('a', 'book-a', 'HTML note', 'note', '<p>你好<br>世界</p>', 'file:///private/hidden'),
        ('b', 'book-a', 'Plain text', 'text', '# Literal <b>tag</b>', NULL),
        ('c', 'book-a', 'Linked attachment', 'file', 'private linked text', 'file:///private/document');
  `)

  const exported = buildNoteTransfer(db)
  expect(exported.count).toBe(3)
  expect(exported.skipped).toBe(1)
  expect(JSON.parse(exported.content)).toEqual({
    format: 'uebox-notes',
    version: 1,
    source: 'electron-public-notes',
    notebooks: [{ identity: 'book-a', title: 'Notebook A' }],
    notes: [
      {
        identity: 'note:9007199254740993',
        notebookIdentity: null,
        title: 'Standalone',
        content: 'First & second\nThird'
      },
      {
        identity: 'source:a',
        notebookIdentity: 'book-a',
        title: 'HTML note',
        content: '你好\n世界'
      },
      {
        identity: 'source:b',
        notebookIdentity: 'book-a',
        title: 'Plain text',
        content: '# Literal <b>tag</b>'
      }
    ]
  })
  expect(exported.content).not.toMatch(/\/private|private-tag|secret\(|Linked attachment/)
  expect(db.prepare('SELECT count(*) AS count FROM notebook_sources').get()).toEqual({ count: 3 })
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
