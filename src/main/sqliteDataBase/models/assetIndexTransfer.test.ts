// @vitest-environment node
import Database from 'better-sqlite3'
import { Buffer } from 'node:buffer'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { applyVaultSchema } from '../vaultSchema'
import {
  buildAssetIndexTransfer,
  InvalidAssetIndexTransferError,
  type AssetIndexTransferDocument
} from './assetIndexTransfer'

const vault = Object.freeze({ id: 'vault-fixture', name: 'Fixture vault', path: '/fixture/vault' })
const maxBytes = 16 * 1024 * 1024
let db: Database.Database

beforeEach(() => {
  vi.spyOn(console, 'log').mockImplementation(() => undefined)
  db = new Database(':memory:')
  applyVaultSchema(db)
})
afterEach(() => {
  vi.restoreAllMocks()
  db.close()
})

function document(): AssetIndexTransferDocument {
  return JSON.parse(buildAssetIndexTransfer(vault, db).content) as AssetIndexTransferDocument
}

function folder(key: string, parent: string | null, name = 'Same name', deleted = 0): void {
  db.prepare(
    'INSERT INTO assetFolder(folderKey, fatherKey, folderName, type, isDelete) VALUES (?, ?, ?, ?, ?)'
  ).run(key, parent, name, 'folder', deleted)
}

function asset(key: string, parent: string | null, path: string | null, deleted = 0): void {
  db.prepare(
    `INSERT INTO assetData(assetKey, folderKey, assetName, filePath, isDelete)
     VALUES (?, ?, ?, ?, ?)`
  ).run(key, parent ?? 'missing-folder', 'Same name', path, deleted)
}

it('projects the real vault tables with stable keys, exact metadata, and no hidden columns', () => {
  folder('a', 'ALL')
  folder('b', 'a')
  asset('asset-a', 'a', 'assetData/2026/Game/Hero.uasset')
  asset('asset-b', 'b', '/fixture/vault/assetData/2026/Game/Hero.uasset')
  db.prepare(
    `UPDATE assetData SET fileSize = ?, size = ?, fileExtension = ?, ext = ?, modifiedTime = ?,
     tags = ?, originPath = ?, imports = ?, assetConfig = ?, pluginInfo = ?, note = ?,
     imgLocalPath = ?, baiduyunPath = ?, webdavPath = ? WHERE assetKey = ?`
  ).run(
    42,
    999,
    'uasset',
    'wrong',
    '2026-11-20 12:34:56',
    '["角色","Hero","角色"]',
    '/private/original',
    'hidden-imports',
    'hidden-config',
    'hidden-plugin',
    'hidden-content',
    '/private/thumbnail',
    'hidden-cloud-path',
    'hidden-webdav-path',
    'asset-a'
  )
  db.exec(`
    CREATE TABLE credentials(secret TEXT);
    INSERT INTO credentials VALUES ('hidden-token');
    ALTER TABLE assetData ADD COLUMN bearer TEXT;
    UPDATE assetData SET bearer = 'hidden-bearer';
    CREATE TEMP TABLE assetData(secret TEXT);
    INSERT INTO temp.assetData VALUES ('hidden-temp-table');
  `)
  const before = db.prepare('SELECT * FROM main.assetData ORDER BY assetKey').all()
  db.pragma('query_only = ON')
  const first = buildAssetIndexTransfer(vault, db)
  const output = JSON.parse(first.content) as AssetIndexTransferDocument
  expect(first).toMatchObject({ folderCount: 3, assetCount: 2 })
  expect(output).toEqual({
    format: 'uebox-local-asset-index',
    version: 1,
    source: 'electron-local-vault',
    vault: { identity: vault.id, name: vault.name, rootPath: vault.path, indexAvailable: true },
    folders: [
      { identity: 'ALL', parentIdentity: null, name: 'ALL' },
      { identity: 'a', parentIdentity: 'ALL', name: 'Same name' },
      { identity: 'b', parentIdentity: 'a', name: 'Same name' }
    ],
    assets: [
      {
        identity: 'asset-a',
        folderIdentity: 'a',
        name: 'Same name',
        relativePath: 'assetData/2026/Game/Hero.uasset',
        size: 42,
        extension: 'uasset',
        modifiedTime: '2026-11-20 12:34:56',
        tags: ['角色', 'Hero']
      },
      {
        identity: 'asset-b',
        folderIdentity: 'b',
        name: 'Same name',
        relativePath: 'assetData/2026/Game/Hero.uasset',
        size: null,
        extension: null,
        modifiedTime: null,
        tags: []
      }
    ]
  })
  expect(buildAssetIndexTransfer(vault, db).content).toBe(first.content)
  expect(first.content).not.toMatch(/hidden-|\/private|imports|bearer|assetConfig|originPath/)
  expect(db.prepare('SELECT * FROM main.assetData ORDER BY assetKey').all()).toEqual(before)
})

it('keeps identities across renames and relocation and bounds every streamed data query', () => {
  folder('folder-key', 'ALL', 'Before')
  asset('asset-key', 'folder-key', 'assetData/file.txt')
  const prepare = vi.spyOn(db, 'prepare')
  const before = document()
  const scans = prepare.mock.calls
    .map(([sql]) => sql)
    .filter((sql) => /FROM main\."asset/.test(sql))
  expect(scans).toHaveLength(3)
  for (const sql of scans) {
    expect(sql).toMatch(/LIMIT \?/)
    expect(sql).not.toMatch(/SELECT \*|imports|assetConfig|pluginInfo|imgLocalPath/)
  }
  db.prepare('UPDATE assetFolder SET folderName = ? WHERE folderKey = ?').run('After', 'folder-key')
  db.prepare('UPDATE assetData SET assetName = ?').run('After')
  const after = JSON.parse(
    buildAssetIndexTransfer({ ...vault, name: 'Renamed', path: '/moved/vault' }, db).content
  ) as AssetIndexTransferDocument
  expect(after.vault.identity).toBe(before.vault.identity)
  expect(after.folders.map((item) => item.identity)).toEqual(
    before.folders.map((item) => item.identity)
  )
  expect(after.assets[0]).toMatchObject({
    identity: 'asset-key',
    folderIdentity: 'folder-key',
    name: 'After'
  })
})

it.each([
  'relative',
  'C:relative',
  'file:///fixture/vault',
  '\\\\?\\C:\\Vault',
  '//server/./vault',
  '/fixture/../vault'
])('rejects an ambiguous registration root: %s', (path) => {
  expect(() => buildAssetIndexTransfer({ ...vault, path }, null)).toThrowError(
    new InvalidAssetIndexTransferError('invalid_source')
  )
})

it('exports offline registration without using a database and does not treat invalid databases as offline', () => {
  expect(JSON.parse(buildAssetIndexTransfer(vault, null).content)).toEqual({
    format: 'uebox-local-asset-index',
    version: 1,
    source: 'electron-local-vault',
    vault: { identity: vault.id, name: vault.name, rootPath: vault.path, indexAvailable: false },
    folders: [],
    assets: []
  })
  db.exec('DROP TABLE assetData; CREATE VIEW assetData AS SELECT 1 AS assetKey')
  expect(() => buildAssetIndexTransfer(vault, db)).toThrowError(
    new InvalidAssetIndexTransferError('invalid_source')
  )
})

it('skips soft deletion and roots orphaned records without using logical paths as physical locations', () => {
  // Reproduce a legacy index with dangling links, without relaxing the production reader.
  db.pragma('foreign_keys = OFF')
  folder('deleted', 'ALL', 'Deleted folder', 1)
  folder('orphan', 'missing')
  folder('child', 'deleted')
  asset('deleted-asset', 'ALL', 'deleted.txt', 1)
  asset('orphan-asset', 'deleted', '/fixture/vault-copy/outside.txt')
  asset('loose-asset', null, null)
  db.prepare('UPDATE assetFolder SET fullPath = ? WHERE folderKey = ?').run(
    '/fixture/vault/pretend-physical',
    'child'
  )
  const output = document()
  expect(output.folders).toEqual([
    { identity: 'ALL', parentIdentity: null, name: 'ALL' },
    { identity: 'child', parentIdentity: null, name: 'Same name' },
    { identity: 'orphan', parentIdentity: null, name: 'Same name' }
  ])
  expect(output.assets).toHaveLength(2)
  expect(
    output.assets.every((item) => item.folderIdentity === null && item.relativePath === null)
  ).toBe(true)
  expect(JSON.stringify(output)).not.toContain('pretend-physical')
})

it.each([
  ['/fixture/vault', '/fixture/vault/Folder/Hero.uasset', 'Folder/Hero.uasset'],
  ['/fixture/vault/', './Folder/Hero.uasset', 'Folder/Hero.uasset'],
  ['/fixture/vault', '/fixture/Vault/Folder/Hero.uasset', null],
  ['/fixture/vault', '/fixture/vault-other/file', null],
  ['/fixture/vault', '/fixture/vault/../outside', null],
  ['/fixture/vault', 'Folder/../../outside', null],
  ['/fixture/vault', 'Folder/../file', null],
  ['/fixture/vault', '/fixture/vault', null],
  ['/fixture/vault', 'file:///fixture/vault/file', null],
  ['/fixture/vault', 'https://example.invalid/file', null],
  ['/fixture/vault', 'file\0.txt', null],
  ['/fixture/vault', 'Folder/file:stream', null],
  ['/fixture/vault', 'Folder/C1\u0085.txt', null],
  ['/fixture/vault', 'x'.repeat(8193), null],
  ['/fixture/vault', 'C:\\fixture\\vault\\file', null],
  ['/fixture/vault', 'Folder\\file', null],
  ['C:\\Vault', 'c:/vault/Folder/Hero.uasset', 'Folder/Hero.uasset'],
  ['C:\\Vault', 'assetData\\2026\\Hero.uasset', 'assetData/2026/Hero.uasset'],
  ['C:\\Vault', 'D:\\Vault\\file', null],
  ['C:\\Vault', 'C:relative.txt', null],
  ['C:\\Vault', '\\Vault\\file', null],
  ['C:\\Vault', '/Vault/file', null],
  ['C:\\Vault', '\\\\?\\C:\\Vault\\file', null],
  ['C:\\Vault', '\\\\.\\C:\\Vault\\file', null],
  ['C:\\Vault', 'C:\\Vault\\file:stream', null],
  ['C:\\Vault', 'C:\\Vault\\dir.\\file', null],
  ['C:\\Vault', 'C:\\Vault\\NUL.txt', null],
  ['\\\\Server\\Share\\Vault', '//server/share/vault/Case/File', 'Case/File'],
  ['\\\\Server\\Share\\Vault', '//server/other/vault/file', null],
  ['/', '/Folder/file', 'Folder/file'],
  ['C:\\', 'C:\\Folder\\file', 'Folder/file']
] as const)('proves paths lexically across operating systems: %s + %s', (root, path, expected) => {
  asset('path-fixture', 'ALL', path)
  const result = buildAssetIndexTransfer({ ...vault, path: root }, db)
  expect((JSON.parse(result.content) as AssetIndexTransferDocument).assets[0].relativePath).toBe(
    expected
  )
})

it('uses originPath only when filePath is absent and preserves nullable legacy metadata', () => {
  asset('a', 'ALL', null)
  asset('b', 'ALL', '/outside/current.txt')
  db.prepare(`UPDATE assetData SET originPath = ?, size = ?, ext = ?, fileSize = ?, tags = ?`).run(
    '/fixture/vault/Old/File.txt',
    23,
    'txt',
    -1,
    ''
  )
  const output = document()
  expect(output.assets[0]).toMatchObject({
    relativePath: 'Old/File.txt',
    size: 23,
    extension: 'txt',
    tags: []
  })
  expect(output.assets[1].relativePath).toBeNull()
})

it('supports older schemas without optional columns without altering them', () => {
  db.exec(`
    DROP TABLE assetData; DROP TABLE assetFolder;
    CREATE TABLE assetFolder(folderKey TEXT UNIQUE, fatherKey TEXT, folderName TEXT);
    CREATE TABLE assetData(assetKey TEXT UNIQUE, folderKey TEXT, assetName TEXT);
    INSERT INTO assetFolder VALUES ('old-folder', NULL, 'Old folder');
    INSERT INTO assetData VALUES ('old-asset', 'old-folder', 'Old asset');
  `)
  db.pragma('query_only = ON')
  expect(document().assets).toEqual([
    {
      identity: 'old-asset',
      folderIdentity: 'old-folder',
      name: 'Old asset',
      relativePath: null,
      size: null,
      extension: null,
      modifiedTime: null,
      tags: []
    }
  ])
  expect(db.prepare('PRAGMA main.table_info(assetData)').all()).toHaveLength(3)
})

it.each(['["valid",{"secret":"hidden"}]', '{"secret":"hidden"}', 'not-json'])(
  'rejects malformed tags without exposing their payload: %s',
  (tags) => {
    asset('bad-tags', 'ALL', 'file')
    db.prepare('UPDATE assetData SET tags = ?').run(tags)
    expect(() => buildAssetIndexTransfer(vault, db)).toThrowError(
      new InvalidAssetIndexTransferError('invalid_source')
    )
  }
)

it('accepts the Rust importer byte boundaries for every exported field', () => {
  const identity = `${'界'.repeat(85)}x`
  const name = `${'界'.repeat(341)}x`
  const root = `/${'r'.repeat(8191)}`
  const path = 'p'.repeat(8192)
  const tag = 'é'.repeat(128)
  const tagList = Array.from(
    { length: 64 },
    (_, index) => `${index.toString().padStart(2, '0')}${tag.slice(1)}`
  )
  folder(identity, 'ALL', name)
  asset(identity, identity, path)
  db.prepare(
    'UPDATE assetData SET assetName = ?, fileExtension = ?, modifiedTime = ?, tags = ?'
  ).run(name, 'e'.repeat(64), 'm'.repeat(64), JSON.stringify(tagList))
  const output = JSON.parse(
    buildAssetIndexTransfer({ id: identity, name, path: root }, db).content
  ) as AssetIndexTransferDocument
  expect(output.vault).toMatchObject({ identity, name, rootPath: root })
  expect(output.folders[1]).toMatchObject({ identity, name })
  expect(output.assets[0]).toMatchObject({ identity, name, relativePath: path, tags: tagList })
  expect(Buffer.byteLength(output.assets[0].tags[0])).toBe(256)
})

it.each([
  { field: 'vault identity', value: 'é'.repeat(129) },
  { field: 'vault name', value: '界'.repeat(342) },
  { field: 'root path', value: `/${'r'.repeat(8192)}` },
  { field: 'asset identity', value: 'i'.repeat(257) },
  { field: 'asset name', value: 'n'.repeat(1025) },
  { field: 'folder identity', value: 'i'.repeat(257) },
  { field: 'folder name', value: 'n'.repeat(1025) },
  { field: 'vault name', value: 'C0\nname' },
  { field: 'vault name', value: 'C1\u0085name' },
  { field: 'vault name', value: 'unpaired\ud800' },
  { field: 'asset name', value: 'C1\u009fname' },
  { field: 'folder identity', value: 'C1\u0080identity' }
])(
  'rejects invalid required text without writing an unreadable manifest: $field',
  ({ field, value }) => {
    const registration =
      field.startsWith('vault') || field === 'root path'
        ? {
            ...vault,
            ...(field === 'vault identity'
              ? { id: value }
              : field === 'vault name'
                ? { name: value }
                : { path: value })
          }
        : vault
    folder(
      field === 'folder identity' ? value : 'folder',
      'ALL',
      field === 'folder name' ? value : 'Folder'
    )
    asset(field === 'asset identity' ? value : 'asset', 'ALL', null)
    if (field === 'asset name') db.prepare('UPDATE assetData SET assetName = ?').run(value)
    expect(() => buildAssetIndexTransfer(registration, db)).toThrowError(
      new InvalidAssetIndexTransferError('invalid_source')
    )
  }
)

it.each([
  { kind: 'count', tagList: Array.from({ length: 65 }, (_, index) => `tag-${index}`) },
  { kind: 'bytes', tagList: ['é'.repeat(129)] },
  { kind: 'control', tagList: ['control\u0085tag'] }
])('rejects tags outside the importer limits: $kind', ({ tagList }) => {
  asset('tags', 'ALL', null)
  db.prepare('UPDATE assetData SET tags = ?').run(JSON.stringify(tagList))
  expect(() => buildAssetIndexTransfer(vault, db)).toThrowError(
    new InvalidAssetIndexTransferError('invalid_source')
  )
})

it('retains assets while replacing unsafe optional metadata and paths with null', () => {
  asset('optional', 'ALL', 'Folder/file:stream')
  db.prepare('UPDATE assetData SET fileExtension = ?, ext = ?, modifiedTime = ?, fileSize = ?').run(
    'e'.repeat(65),
    'bad\u0085ext',
    'm'.repeat(65),
    9_007_199_254_740_992
  )
  expect(document().assets[0]).toMatchObject({
    identity: 'optional',
    relativePath: null,
    extension: null,
    modifiedTime: null,
    size: null
  })
  db.prepare('UPDATE assetData SET modifiedTime = ?').run('C1\u0085time')
  expect(document().assets[0].modifiedTime).toBeNull()
})

it('rejects cyclic folder relationships rather than emitting an unusable tree', () => {
  folder('a', null)
  folder('b', 'a')
  db.prepare('UPDATE assetFolder SET fatherKey = ? WHERE folderKey = ?').run('b', 'a')
  expect(() => buildAssetIndexTransfer(vault, db)).toThrowError(
    new InvalidAssetIndexTransferError('invalid_source')
  )
})

it.each([
  { kind: 'asset', maximum: 32_768, code: 'asset_limit' },
  { kind: 'folder', maximum: 8192, code: 'folder_limit' }
] as const)(
  'accepts the $kind boundary and rejects one more live record',
  ({ kind, maximum, code }) => {
    const statement =
      kind === 'asset'
        ? db.prepare('INSERT INTO assetData(assetKey, folderKey, assetName) VALUES (?, ?, ?)')
        : db.prepare(
            'INSERT INTO assetFolder(folderKey, fatherKey, folderName, type) VALUES (?, ?, ?, ?)'
          )
    const rows = maximum - (kind === 'folder' ? 1 : 0)
    db.transaction(() => {
      for (let index = 0; index < rows; index++) {
        if (kind === 'asset') statement.run(`asset-${index}`, 'ALL', 'Asset')
        else statement.run(`folder-${index}`, 'ALL', 'Folder', 'folder')
      }
    })()
    const result = buildAssetIndexTransfer(vault, db)
    expect(kind === 'asset' ? result.assetCount : result.folderCount).toBe(maximum)
    if (kind === 'asset') statement.run('overflow', 'ALL', 'Asset')
    else statement.run('overflow', 'ALL', 'Folder', 'folder')
    expect(() => buildAssetIndexTransfer(vault, db)).toThrowError(
      new InvalidAssetIndexTransferError(code)
    )
  }
)

it('counts UTF-8 JSON including escaping and the final newline at the exact 16 MiB boundary', () => {
  const insert = db.prepare(
    'INSERT INTO assetData(assetKey, folderKey, assetName) VALUES (?, ?, ?)'
  )
  db.transaction(() => {
    for (let index = 0; index < 16_000; index++) insert.run(`asset-${index}`, 'ALL', '')
  })()
  const update = db.prepare('UPDATE assetData SET assetName = ? WHERE assetKey = ?')
  update.run('Quote "', 'asset-0')
  let remaining = maxBytes - Buffer.byteLength(buildAssetIndexTransfer(vault, db).content)
  db.transaction(() => {
    for (let index = 1; remaining > 0; index++) {
      const bytes = Math.min(1024, remaining)
      update.run(`${'界'.repeat(Math.floor(bytes / 3))}${'x'.repeat(bytes % 3)}`, `asset-${index}`)
      remaining -= bytes
      expect(index).toBeLessThan(15_999)
    }
  })()
  expect(Buffer.byteLength(buildAssetIndexTransfer(vault, db).content)).toBe(maxBytes)
  update.run('x', 'asset-15999')
  expect(() => buildAssetIndexTransfer(vault, db)).toThrowError(
    new InvalidAssetIndexTransferError('byte_limit')
  )
})

it('rejects oversized selected fields in SQL before materializing them and hides database errors', () => {
  asset('huge', 'ALL', null)
  db.prepare('UPDATE assetData SET tags = ?').run('x'.repeat(maxBytes + 1))
  expect(() => buildAssetIndexTransfer(vault, db)).toThrowError(
    new InvalidAssetIndexTransferError('byte_limit')
  )
  vi.spyOn(db, 'prepare').mockImplementation(() => {
    throw new Error('hidden-secret /private/vault.db')
  })
  expect(() => buildAssetIndexTransfer(vault, db)).toThrowError(
    new InvalidAssetIndexTransferError('invalid_source')
  )
})
