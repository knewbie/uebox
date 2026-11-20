/** @vitest-environment node */
import Database from 'better-sqlite3'
import * as fs from 'node:fs/promises'
import { tmpdir } from 'node:os'
import * as path from 'node:path'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { registerProjectRegistrationExportIPC } from './projectRegistrationExport'
import { buildProjectRegistrationExport } from '../models/projectRegistrationExport'

const mocks = vi.hoisted(() => ({
  handlers: new Map<string, (...args: unknown[]) => Promise<unknown>>(),
  getPublicDatabase: vi.fn(),
  showSaveDialog: vi.fn(),
  fromWebContents: vi.fn(),
  sql: vi.fn()
}))

vi.mock('electron', () => ({
  ipcMain: {
    handle: (channel: string, handler: (...args: unknown[]) => Promise<unknown>) =>
      mocks.handlers.set(channel, handler)
  },
  dialog: { showSaveDialog: mocks.showSaveDialog },
  BrowserWindow: { fromWebContents: mocks.fromWebContents }
}))
vi.mock('../index', () => ({ getPublicDatabase: mocks.getPublicDatabase }))
vi.mock('node:fs/promises', async (importOriginal) => {
  const actual = await importOriginal<typeof import('node:fs/promises')>()
  return { ...actual, open: vi.fn(actual.open) }
})

const LIMIT_BYTES = 1_048_576
const hostFamily = process.platform === 'win32' ? 'windows' : 'posix'
const event = { sender: { isDestroyed: () => false } }
let root: string
let database: Database.Database

beforeEach(async () => {
  vi.clearAllMocks()
  root = await fs.mkdtemp(path.join(tmpdir(), 'uebox-registration-export-'))
  database = new Database(path.join(root, 'source.sqlite'), { verbose: mocks.sql })
  database.pragma('journal_mode = WAL')
  database.pragma('wal_autocheckpoint = 0')
  database.exec(`
    CREATE TABLE projects (
      id INTEGER PRIMARY KEY,
      projectName TEXT,
      projectPath TEXT,
      originPath TEXT,
      projectData TEXT,
      projectConfig TEXT,
      note TEXT
    );
    CREATE TABLE credentials (secret TEXT);
    INSERT INTO credentials VALUES ('fixture-credential-must-not-export');
  `)
  mocks.getPublicDatabase.mockReturnValue(database)
  mocks.showSaveDialog.mockResolvedValue({
    canceled: false,
    filePath: path.join(root, 'export.json')
  })
  mocks.fromWebContents.mockReturnValue(null)
  mocks.handlers.clear()
  registerProjectRegistrationExportIPC()
  mocks.sql.mockClear()
})

afterEach(async () => {
  vi.mocked(fs.open).mockRestore()
  if (database.open) database.close()
  await fs.rm(root, { recursive: true, force: true })
})

function insert(projectName: unknown, projectPath: unknown, originPath: unknown = null): void {
  database
    .prepare(
      'INSERT INTO projects (projectName, projectPath, originPath, projectData, projectConfig, note) VALUES (?, ?, ?, ?, ?, ?)'
    )
    .run(
      projectName,
      projectPath,
      originPath,
      'fixture-private-data',
      'fixture-private-config',
      'fixture-private-note'
    )
}

function invoke(): Promise<unknown> {
  const handler = mocks.handlers.get('db:project:exportRegistration')
  if (!handler) throw new Error('Registration export handler missing')
  return handler(event)
}

it('exports only the three projected columns from the active WAL connection and keeps offline projects', async () => {
  const offline = path.join(root, 'missing-project')
  const fallback = path.join(root, 'missing-fallback')
  insert('离线工程', offline)
  insert('  Fallback name  ', null, path.join(fallback, 'Game.uproject'))
  const beforeMain = await fs.readFile(path.join(root, 'source.sqlite'))
  const beforeWal = await fs.readFile(path.join(root, 'source.sqlite-wal'))
  expect(beforeWal.byteLength).toBeGreaterThan(32)
  mocks.sql.mockClear()
  const parent = { fixture: true }
  mocks.fromWebContents.mockReturnValueOnce(parent)

  expect(await invoke()).toEqual({ success: true, code: 'project_registration_exported', count: 2 })
  expect(mocks.showSaveDialog).toHaveBeenCalledWith(
    parent,
    expect.objectContaining({
      defaultPath: 'uebox-project-registration.json',
      filters: [{ name: 'JSON', extensions: ['json'] }]
    })
  )
  const content = await fs.readFile(path.join(root, 'export.json'), 'utf8')
  expect(JSON.parse(content)).toEqual({
    format: 'uebox-project-registration',
    version: 1,
    pathFamily: hostFamily,
    projects: [
      { engine: 'unreal', name: '离线工程', path: offline },
      { engine: 'unreal', name: '  Fallback name  ', path: fallback }
    ]
  })
  expect(content).not.toContain('fixture-private')
  expect(content).not.toContain('fixture-credential')
  expect(mocks.sql.mock.calls.map(([sql]) => sql)).toEqual([
    expect.stringMatching(
      /^SELECT projectName, projectPath, originPath FROM projects ORDER BY id ASC LIMIT 4097(?:\.0)?$/
    )
  ])
  expect(await fs.readFile(path.join(root, 'source.sqlite'))).toEqual(beforeMain)
  expect(await fs.readFile(path.join(root, 'source.sqlite-wal'))).toEqual(beforeWal)
  expect(await fs.readdir(root)).toEqual(expect.arrayContaining(['export.json', 'source.sqlite']))
  await expect(fs.stat(offline)).rejects.toMatchObject({ code: 'ENOENT' })
  if (process.platform !== 'win32') {
    expect((await fs.stat(path.join(root, 'export.json'))).mode & 0o777).toBe(0o600)
  }
})

it.each([
  ['windows', 'D:\\Offline\\Game', null, 'D:\\Offline\\Game'],
  ['windows', null, 'D:/离线/Game/Game.UPROJECT', 'D:/离线/Game'],
  ['windows', null, '\\\\server\\share\\Game\\Game.uproject', '\\\\server\\share\\Game'],
  ['posix', '/offline/工程', null, '/offline/工程'],
  ['posix', '', '/offline/工程/Game.uproject', '/offline/工程']
] as const)(
  'uses only lexical %s path handling without probing projects (%#)',
  (family, directory, origin, expected) => {
    insert('Name', directory, origin)
    const document = JSON.parse(buildProjectRegistrationExport(database, family).content)
    expect(document.projects).toEqual([{ engine: 'unreal', name: 'Name', path: expected }])
  }
)

it.each([
  ['posix', null, '/offline/离线工程', null, '离线工程'],
  ['posix', '   ', '/offline/Game/', null, 'Game'],
  ['posix', null, null, '/offline/Fallback/Game.uproject', 'Fallback'],
  ['posix', null, '/', '/RootGame.uproject', 'RootGame'],
  ['windows', null, 'D:\\', 'D:\\RootGame.UPROJECT', 'RootGame'],
  ['windows', null, '\\\\server\\share\\', '\\\\server\\share\\RootGame.uproject', 'RootGame']
] as const)(
  'derives a missing name lexically for %s registrations (%#)',
  (family, name, directory, origin, expected) => {
    insert(name, directory, origin)
    const document = JSON.parse(buildProjectRegistrationExport(database, family).content)
    expect(document.projects[0].name).toBe(expected)
  }
)

it.each([
  [null, '/', null],
  ['   ', '/', '/Game.txt'],
  ['Bad\0name', '/offline/Game', null],
  ['Name', 'relative/Game', null],
  ['Name', null, 'Game.uproject'],
  ['Name', null, '/offline/Game.txt'],
  ['Name', '/offline/Bad\0path', null],
  ['Name', null, null]
])('rejects invalid rows without a dialog or output (%#)', async (name, directory, origin) => {
  insert(name, directory, origin)
  const family = 'posix'
  expect(() => buildProjectRegistrationExport(database, family)).toThrow()
  expect(await invoke()).toEqual({ success: false, code: 'project_registration_export_invalid' })
  expect(mocks.showSaveDialog).not.toHaveBeenCalled()
  await expect(fs.stat(path.join(root, 'export.json'))).rejects.toMatchObject({ code: 'ENOENT' })
})

it.each(['C:relative', '\\root-relative', '\\\\?\\C:\\Game', '\\\\.\\pipe\\Game'])(
  'rejects nonportable Windows roots: %s',
  (directory) => {
    insert('Name', directory)
    expect(() => buildProjectRegistrationExport(database, 'windows')).toThrow()
  }
)

it('exports an empty registration list as a valid document', async () => {
  expect(await invoke()).toEqual({ success: true, code: 'project_registration_exported', count: 0 })
  expect(JSON.parse(await fs.readFile(path.join(root, 'export.json'), 'utf8')).projects).toEqual([])
})

it('returns cancellation without creating a file and allows a subsequent export', async () => {
  mocks.showSaveDialog.mockResolvedValueOnce({ canceled: true })
  expect(await invoke()).toEqual({ success: false, code: 'project_registration_export_cancelled' })
  await expect(fs.stat(path.join(root, 'export.json'))).rejects.toMatchObject({ code: 'ENOENT' })
  expect(await invoke()).toMatchObject({ success: true })
})

it('refuses an existing file even after a native dialog returns its path', async () => {
  const output = path.join(root, 'export.json')
  await fs.writeFile(output, 'keep-existing-file')
  expect(await invoke()).toEqual({ success: false, code: 'project_registration_export_exists' })
  expect(await fs.readFile(output, 'utf8')).toBe('keep-existing-file')
})

it.skipIf(process.platform === 'win32')(
  'refuses a symlink without modifying its target',
  async () => {
    const target = path.join(root, 'existing-target.json')
    await fs.writeFile(target, 'keep-symlink-target')
    await fs.symlink(target, path.join(root, 'export.json'))
    expect(await invoke()).toEqual({ success: false, code: 'project_registration_export_exists' })
    expect(await fs.readFile(target, 'utf8')).toBe('keep-symlink-target')
  }
)

it('returns bounded codes for database, dialog and write failures without leaking exception text', async () => {
  const sensitive = new Error('fixture-secret /private/database.sqlite SQL SELECT *')
  mocks.getPublicDatabase.mockImplementationOnce(() => {
    throw sensitive
  })
  expect(await invoke()).toEqual({
    success: false,
    code: 'project_registration_export_unavailable'
  })
  mocks.showSaveDialog.mockRejectedValueOnce(sensitive)
  expect(await invoke()).toEqual({
    success: false,
    code: 'project_registration_export_unavailable'
  })
  mocks.showSaveDialog.mockResolvedValueOnce({
    canceled: false,
    filePath: path.join(root, 'missing', 'export.json')
  })
  expect(await invoke()).toEqual({
    success: false,
    code: 'project_registration_export_write_failed'
  })
})

it('does not admit another export while a native save dialog is open', async () => {
  let finish!: (value: { canceled: boolean }) => void
  mocks.showSaveDialog.mockReturnValueOnce(
    new Promise((resolve) => {
      finish = resolve
    })
  )
  const first = invoke()
  expect(await invoke()).toEqual({ success: false, code: 'project_registration_export_busy' })
  expect(mocks.showSaveDialog).toHaveBeenCalledTimes(1)
  finish({ canceled: true })
  expect(await first).toEqual({ success: false, code: 'project_registration_export_cancelled' })
})

it('accepts 4096 registrations and rejects 4097 without creating output', async () => {
  const statement = database.prepare(
    'INSERT INTO projects (projectName, projectPath) VALUES (?, ?)'
  )
  database.transaction(() => {
    for (let index = 0; index < 4096; index += 1)
      statement.run(`Project ${index}`, path.join(root, `offline-${index}`))
  })()
  expect(buildProjectRegistrationExport(database, hostFamily).count).toBe(4096)
  statement.run('Overflow', path.join(root, 'overflow'))
  expect(await invoke()).toEqual({ success: false, code: 'project_registration_export_invalid' })
  expect(mocks.showSaveDialog).not.toHaveBeenCalled()
})

it('measures the actual UTF-8 file including its newline at the 1 MiB boundary', async () => {
  insert('x', path.join(root, 'offline'))
  const base = buildProjectRegistrationExport(database, hostFamily).content
  const name = 'x'.repeat(LIMIT_BYTES - Buffer.byteLength(base, 'utf8') + 1)
  database.prepare('UPDATE projects SET projectName = ?').run(name)
  expect(await invoke()).toEqual({ success: true, code: 'project_registration_exported', count: 1 })
  expect((await fs.stat(path.join(root, 'export.json'))).size).toBe(LIMIT_BYTES)
  database.prepare('UPDATE projects SET projectName = ?').run(`${name}中`)
  expect(await invoke()).toEqual({ success: false, code: 'project_registration_export_invalid' })
  expect(mocks.showSaveDialog).toHaveBeenCalledTimes(1)
  expect((await fs.stat(path.join(root, 'export.json'))).size).toBe(LIMIT_BYTES)
})

it('rejects a multibyte document smaller than 1 MiB in characters but larger in UTF-8 bytes', async () => {
  insert('中'.repeat(350_000), path.join(root, 'offline'))
  expect(await invoke()).toEqual({ success: false, code: 'project_registration_export_invalid' })
  expect(mocks.showSaveDialog).not.toHaveBeenCalled()
})

it('does not report success if writing to the exclusively created file fails', async () => {
  const open = vi.mocked(fs.open)
  open.mockRejectedValueOnce(
    Object.assign(new Error('fixture-secret write failure'), { code: 'EIO' })
  )
  expect(await invoke()).toEqual({
    success: false,
    code: 'project_registration_export_write_failed'
  })
  expect(await invoke()).toMatchObject({ success: true })
})
