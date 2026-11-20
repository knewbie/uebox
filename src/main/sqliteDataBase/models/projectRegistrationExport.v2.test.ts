/** @vitest-environment node */
import Database from 'better-sqlite3'
import { afterEach, beforeEach, expect, it } from 'vitest'
import {
  buildProjectRegistrationExportV2,
  InvalidProjectRegistrationError
} from './projectRegistrationExport'

let db: Database.Database

beforeEach(() => {
  db = new Database(':memory:')
  db.exec(`
    CREATE TABLE projects (
      id INTEGER PRIMARY KEY,
      projectName TEXT,
      projectPath TEXT,
      originPath TEXT,
      projectData TEXT,
      projectConfig TEXT,
      image TEXT,
      coverMode TEXT,
      note TEXT,
      isPinned INTEGER NOT NULL DEFAULT 0,
      sort_order REAL NOT NULL DEFAULT 0.0
    );
    CREATE TABLE credentials (secret TEXT);
    INSERT INTO credentials VALUES ('fixture-credential-must-not-export');
  `)
})

afterEach(() => db.close())

function insert(
  name: string | null,
  path: string | null,
  note: string | null = null,
  pinned: number = 0,
  sortOrder: number = 0
): void {
  db.prepare(
    `INSERT INTO projects (projectName, projectPath, originPath, projectData, projectConfig, image, coverMode, note, isPinned, sort_order)
     VALUES (?, ?, NULL, '{"Description":"secret uproject","resourcePath":"/private/assets"}', '[ini] secret config', 'cover.png', 'custom', ?, ?, ?)`
  ).run(name, path, note, pinned, sortOrder)
}

it('exports v2 text metadata without cover or resource-source paths', () => {
  insert('离线工程', '/offline/Game', '我的备注', 1, 2.5)

  const { content, count } = buildProjectRegistrationExportV2(db, 'posix')
  expect(count).toBe(1)
  const parsed = JSON.parse(content)
  expect(parsed.format).toBe('uebox-project-registration')
  expect(parsed.version).toBe(2)
  expect(parsed.pathFamily).toBe('posix')
  expect(parsed.projects).toEqual([
    {
      engine: 'unreal',
      name: '离线工程',
      path: '/offline/Game',
      note: '我的备注',
      pinned: true,
      sortOrder: 2.5
    }
  ])
  expect(content).not.toContain('secret')
  expect(content).not.toContain('cover.png')
  expect(content).not.toContain('/private/assets')
  expect(content).not.toContain('fixture-credential')
})

it('defaults missing legacy metadata columns and keeps the v1 name/path semantics', () => {
  insert('  Fallback name  ', '/offline/Fallback')
  db.exec('ALTER TABLE projects DROP COLUMN isPinned')
  db.exec('ALTER TABLE projects DROP COLUMN sort_order')

  const parsed = JSON.parse(buildProjectRegistrationExportV2(db, 'posix').content)
  expect(parsed.projects[0]).toEqual({
    engine: 'unreal',
    name: '  Fallback name  ',
    path: '/offline/Fallback',
    note: null,
    pinned: false,
    sortOrder: 0
  })
})

it('exports null note, unpinned, and zero sort as false/zero/null', () => {
  insert('Plain', '/offline/Plain')
  const parsed = JSON.parse(buildProjectRegistrationExportV2(db, 'posix').content)
  expect(parsed.projects[0]).toEqual({
    engine: 'unreal',
    name: 'Plain',
    path: '/offline/Plain',
    note: null,
    pinned: false,
    sortOrder: 0
  })
})

it('never emits a description field, even though the target has one', () => {
  insert('WithNote', '/offline/WithNote', 'note text only')
  const parsed = JSON.parse(buildProjectRegistrationExportV2(db, 'posix').content)
  expect(parsed.projects[0]).not.toHaveProperty('description')
  expect(parsed.projects[0].note).toBe('note text only')
  expect(Object.keys(parsed.projects[0]).sort()).toEqual([
    'engine',
    'name',
    'note',
    'path',
    'pinned',
    'sortOrder'
  ])
})

it('normalizes an empty note to null like the target normalize_text', () => {
  insert('EmptyNote', '/offline/EmptyNote', '')
  const parsed = JSON.parse(buildProjectRegistrationExportV2(db, 'posix').content)
  expect(parsed.projects[0].note).toBeNull()
})

it('accepts a note well above 64 KiB, matching the target 1 MiB limit', () => {
  // 200 KiB: rejected under the old 64 KiB export limit, valid for the target
  // project_metadata CHECK (1..=1048576 bytes).
  const note = 'a'.repeat(200 * 1024)
  insert('LargeNote', '/offline/LargeNote', note)
  const parsed = JSON.parse(buildProjectRegistrationExportV2(db, 'posix').content)
  expect(parsed.projects[0].note).toBe(note)
})

it('rejects a note over 1 MiB or containing NUL without output', () => {
  insert('TooLarge', '/offline/TooLarge', '中'.repeat(349_526)) // 1048578 bytes > 1 MiB
  expect(() => buildProjectRegistrationExportV2(db, 'posix')).toThrow(
    InvalidProjectRegistrationError
  )

  db.prepare('DELETE FROM projects').run()
  insert('Nul', '/offline/Nul', 'bad\0note')
  expect(() => buildProjectRegistrationExportV2(db, 'posix')).toThrow(
    InvalidProjectRegistrationError
  )
})
