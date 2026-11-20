import Database from 'better-sqlite3'
import { readFileSync, mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { describe, expect, it } from 'vitest'

import {
  buildProjectCollectionExport,
  InvalidProjectCollectionExportError
} from './projectCollectionExport'

function snapshotSchema(db: Database.Database, legacyColumn = false): void {
  db.exec(`
    CREATE TABLE project_collections (
      id INTEGER PRIMARY KEY, collectionKey TEXT, name TEXT, sort_order INTEGER,
      description TEXT, isPinned INTEGER
    );
    CREATE TABLE projects (
      id INTEGER PRIMARY KEY, projectKey TEXT, projectPath TEXT, originPath TEXT,
      projectName TEXT, projectData TEXT, note TEXT${legacyColumn ? ', collectionKey TEXT' : ''}
    );
  `)
  if (!legacyColumn) {
    db.exec('CREATE TABLE project_collection_items (projectKey TEXT, collectionKey TEXT)')
  }
}

function fixture(run: (db: Database.Database) => void, legacyColumn = false): void {
  const db = new Database(':memory:')
  try {
    snapshotSchema(db, legacyColumn)
    run(db)
  } finally {
    db.close()
  }
}

describe('project collection export', () => {
  it('projects only v1 fields, preserving empty collections and offline paths', () => {
    fixture((db) => {
      db.exec(`
        INSERT INTO project_collections VALUES
          (1, 'many', '教学', 3, 'private description', 1),
          (2, 'empty', NULL, -3, 'private description', 0),
          (3, 'other', 'Other', 3, NULL, 0);
        INSERT INTO projects VALUES
          (1, 'p1', '/offline/Some/../Game', '/different/Fallback.uproject',
            'unused name', 'private project data', 'private note'),
          (2, 'p2', NULL, '/offline/Other/Other.uproject', NULL, NULL, NULL),
          (3, 'p3', '/offline/Some/../Game', NULL, NULL, NULL, NULL),
          (4, 'unreferenced', 'broken relative path', NULL, NULL, NULL, NULL);
        INSERT INTO project_collection_items VALUES
          ('p1', 'many'), ('p2', 'many'), ('p3', 'many'), ('p1', 'other');
      `)
      const result = buildProjectCollectionExport(db, 'posix')
      expect(result.count).toBe(3)
      expect(JSON.parse(result.content)).toEqual({
        format: 'uebox-project-collections',
        version: 1,
        pathFamily: 'posix',
        collections: [
          { id: 'empty', name: null, sortOrder: -3, members: [] },
          {
            id: 'many',
            name: '教学',
            sortOrder: 3,
            members: [
              { engine: 'unreal', path: '/offline/Some/../Game' },
              { engine: 'unreal', path: '/offline/Other' }
            ]
          },
          {
            id: 'other',
            name: 'Other',
            sortOrder: 3,
            members: [{ engine: 'unreal', path: '/offline/Some/../Game' }]
          }
        ]
      })
    })
  })

  it('reads the old single collectionKey column and leaves it in place', () => {
    fixture((db) => {
      db.exec(`
        INSERT INTO project_collections VALUES (1, 'old', 'Old', 0, NULL, 0);
        INSERT INTO projects VALUES
          (1, 'p', NULL, 'Z:\\offline\\Old\\Old.uproject', NULL, NULL, NULL, 'old'),
          (2, 'none', 'invalid', NULL, NULL, NULL, NULL, ' ');
      `)
      const before = db.serialize()
      const { content } = buildProjectCollectionExport(db, 'windows')
      expect(JSON.parse(content).collections).toEqual([
        {
          id: 'old',
          name: 'Old',
          sortOrder: 0,
          members: [{ engine: 'unreal', path: 'Z:\\offline\\Old' }]
        }
      ])
      expect(db.serialize()).toEqual(before)
    }, true)
  })

  it('uses the relation table when both membership layouts exist', () => {
    fixture((db) => {
      db.exec(`
        CREATE TABLE project_collection_items (projectKey TEXT, collectionKey TEXT);
        INSERT INTO project_collections VALUES (1, 'old', 'Old', 0, NULL, 0);
        INSERT INTO projects VALUES (1, 'p', '/offline/path', NULL, NULL, NULL, NULL, 'old');
      `)
      db.defaultSafeIntegers(true)
      const { content } = buildProjectCollectionExport(db, 'posix')
      expect(JSON.parse(content).collections).toEqual([
        { id: 'old', name: 'Old', sortOrder: 0, members: [] }
      ])
    }, true)
  })

  it('reads a caller-owned, physically read-only temporary database and preserves its bytes', () => {
    const directory = mkdtempSync(join(tmpdir(), 'uebox-collection-export-test-'))
    const path = join(directory, 'fixture.sqlite')
    try {
      const writer = new Database(path)
      try {
        snapshotSchema(writer)
      } finally {
        writer.close()
      }
      const before = readFileSync(path)
      const reader = new Database(path, { readonly: true, fileMustExist: true })
      try {
        const result = buildProjectCollectionExport(reader, 'posix')
        expect(result.count).toBe(0)
        expect(JSON.parse(result.content).collections).toEqual([])
        expect(reader.open).toBe(true)
      } finally {
        reader.close()
      }
      expect(readFileSync(path)).toEqual(before)
    } finally {
      rmSync(directory, { recursive: true, force: true })
    }
  })

  it.each([
    [
      'duplicate_id',
      `INSERT INTO project_collections VALUES
        (2, 'c', 'Duplicate', 1, NULL, 0);`
    ],
    ['duplicate_membership', `INSERT INTO project_collection_items VALUES ('p', 'c'), ('p', 'c');`],
    ['dangling_reference', `INSERT INTO project_collection_items VALUES ('missing', 'c');`],
    ['dangling_reference', `INSERT INTO project_collection_items VALUES ('p', 'missing');`],
    ['invalid_collection', `UPDATE project_collections SET sort_order = 9007199254740992;`],
    [
      'invalid_project',
      `UPDATE projects SET projectPath = 'C:relative'; INSERT INTO project_collection_items VALUES ('p', 'c');`
    ],
    [
      'invalid_source',
      'ALTER TABLE projects RENAME TO private_projects; CREATE VIEW projects AS SELECT * FROM private_projects;'
    ],
    ['invalid_source', 'DROP TABLE project_collection_items;']
  ])('reports %s for malformed source data', (code, statement) => {
    fixture((db) => {
      db.exec(`
        INSERT INTO project_collections VALUES (1, 'c', 'Collection', 0, NULL, 0);
        INSERT INTO projects VALUES (1, 'p', 'Z:/offline/Good', NULL, NULL, NULL, NULL);
      `)
      db.exec(statement)
      expect(() => buildProjectCollectionExport(db, 'windows')).toThrow(
        InvalidProjectCollectionExportError
      )
      expect(() => buildProjectCollectionExport(db, 'windows')).toThrow(
        expect.objectContaining({ code })
      )
    })
  })

  it('bounds the collection count, edge count and serialized UTF-8 bytes separately', () => {
    fixture((db) => {
      db.exec(`
        WITH RECURSIVE seq(n) AS (VALUES(1) UNION ALL SELECT n + 1 FROM seq WHERE n < 4097)
        INSERT INTO project_collections (collectionKey, name, sort_order)
        SELECT 'c' || n, 'Name', 0 FROM seq;
      `)
      expect(() => buildProjectCollectionExport(db, 'posix')).toThrow(
        expect.objectContaining({ code: 'collection_limit' })
      )
    })
    fixture((db) => {
      db.exec(`
        INSERT INTO project_collections VALUES (1, 'c', 'C', 0, NULL, 0);
        WITH RECURSIVE seq(n) AS (VALUES(1) UNION ALL SELECT n + 1 FROM seq WHERE n < 16385)
        INSERT INTO project_collection_items SELECT 'p' || n, 'c' FROM seq;
      `)
      expect(() => buildProjectCollectionExport(db, 'posix')).toThrow(
        expect.objectContaining({ code: 'membership_limit' })
      )
    })
    fixture((db) => {
      const insert = db.prepare(
        'INSERT INTO project_collections (collectionKey, name, sort_order) VALUES (?, ?, 0)'
      )
      db.transaction(() => {
        for (let index = 0; index < 1100; index++) insert.run(`c${index}`, '甲'.repeat(340))
      })()
      expect(() => buildProjectCollectionExport(db, 'posix')).toThrow(
        expect.objectContaining({ code: 'byte_limit' })
      )
    })
  })
})
