/** @vitest-environment node */
import { afterEach, beforeEach, expect, it } from 'vitest'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, readdir, rm, symlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { exportVisibleChatHistory } from './chatHistoryExport'
import { fileNameFor } from './chatSessionFiles'

let root: string
let historyDir: string
let destination: string

beforeEach(async () => {
  root = await mkdtemp(join(tmpdir(), 'uebox-visible-chat-export-'))
  historyDir = join(root, 'chat-history')
  destination = join(root, 'export.json')
  await mkdir(join(historyDir, 'sessions'), { recursive: true })
})

afterEach(async () => rm(root, { recursive: true, force: true }))

const chat = (
  id: string,
  title = 'Fixture chat'
): { id: string; title: string; createdAt: number; updatedAt: number } => ({
  id,
  title,
  createdAt: 100,
  updatedAt: 200
})

async function put(path: string, value: unknown): Promise<void> {
  await writeFile(join(historyDir, path), JSON.stringify(value))
}

async function body(id: string, messages: unknown[]): Promise<void> {
  await put(join('sessions', fileNameFor(id)), { sid: id, messages })
}

const unavailable = (kind: string): { kind: string; availability: string } => ({
  kind,
  availability: 'unavailable'
})
const dataUrl = (mediaType: string, bytes: Buffer): string =>
  `data:${mediaType};base64,${bytes.toString('base64')}`
const imagePart = (url: string): { type: string; image_url: { url: string } } => ({
  type: 'image_url',
  image_url: { url }
})
const embedded = (
  mediaType: string,
  bytes: Buffer
): { kind: string; availability: string; mediaType: string; sha256: string; base64: string } => ({
  kind: 'image',
  availability: 'embedded',
  mediaType,
  sha256: createHash('sha256').update(bytes).digest('hex'),
  base64: bytes.toString('base64')
})
const png = Buffer.from(
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+X2ioAAAAASUVORK5CYII=',
  'base64'
)

it('exports the allowlisted visible fields and turns attachment references into placeholders', async () => {
  const id = 'fixture'
  await put('chat-sessions.json', {
    sessions: [
      {
        ...chat(id),
        agentSessionId: 'hidden-agent-id',
        model: { apiKey: 'provider-secret', endpoint: 'secret-provider-endpoint' },
        project: { projectPath: '/private/project' },
        agentHistory: ['private-agent-history']
      }
    ],
    draftsById: { [id]: 'unsent-private-draft' },
    permissionModeById: { [id]: 'hidden-permission-mode' }
  })
  await put(join('sessions', fileNameFor(id)), {
    sid: id,
    historySummary: 'private-summary',
    compressedUserCount: 20,
    messages: [
      {
        role: 'user',
        content: [
          { type: 'text', text: '你好' },
          { type: 'image_url', image_url: { url: 'data:image/png;base64,PRIVATE_BASE64' } },
          { type: 'file_url', file_url: { url: 'file:///private/clip', mimeType: 'video/mp4' } },
          { type: 'tool_result', text: 'hidden-tool-content' },
          { type: 'thinking', text: 'hidden-thinking-content' }
        ],
        excelFiles: [{ fileName: '/private/sheet.xlsx' }]
      },
      {
        role: 'assistant',
        content: [
          'An image',
          '![remote](https://fixture.invalid/asset?signature=hidden-sig)',
          '![saved](local-resource://C:/private/AIGC/%E5%9B%BE%E7%89%87/saved.png)',
          '![inline](data:image/png;base64,PRIVATE_BASE64)'
        ].join('\n\n'),
        thinking: 'hidden-thought',
        toolResults: [{ result: 'hidden-raw-tool-receipt' }],
        agentProcess: [{ type: 'text', text: 'hidden-timeline' }],
        citations: [{ url: 'https://fixture.invalid/?token=hidden-citation-token' }]
      },
      { role: 'assistant', content: 'Interrupted text', status: 'typing' },
      { role: 'assistant', content: '正在思考...', status: 'typing' }
    ]
  })
  await put('follow-up-queue.json', ['ignored-queued-message'])
  await writeFile(join(root, 'ai-provider-secrets.bin'), 'secret-fixture')
  const before = await readFile(join(historyDir, 'chat-sessions.json'))
  const beforeBody = await readFile(join(historyDir, 'sessions', fileNameFor(id)))

  expect(await exportVisibleChatHistory({ historyDir, destination })).toEqual({
    success: true,
    code: 'chat_history_exported',
    count: 1
  })
  const raw = await readFile(destination, 'utf8')
  expect(JSON.parse(raw)).toEqual({
    format: 'uebox-visible-chat-history',
    version: 1,
    chats: [
      {
        ...chat(id),
        messages: [
          {
            role: 'user',
            text: '你好',
            status: 'completed',
            attachments: [unavailable('image'), unavailable('video'), unavailable('excel')]
          },
          {
            role: 'assistant',
            text: 'An image',
            status: 'completed',
            attachments: [unavailable('image'), unavailable('image'), unavailable('image')]
          },
          { role: 'assistant', text: 'Interrupted text', status: 'interrupted' },
          { role: 'assistant', text: '', status: 'interrupted' }
        ]
      }
    ]
  })
  expect(raw).not.toMatch(/hidden-|private|secret|PRIVATE_BASE64|signature|fixture\.invalid/)
  expect(await readFile(join(historyDir, 'chat-sessions.json'))).toEqual(before)
  expect(await readFile(join(historyDir, 'sessions', fileNameFor(id)))).toEqual(beforeBody)
})

it('uses the visible metadata list and falls back to the legacy file without modifying it', async () => {
  const oddId = 'old/route:sid'
  await put('chat-sessions.json', { sessions: [chat(oddId), chat('legacy'), chat('empty')] })
  await put(join('sessions', 'index.json'), {
    version: 1,
    sessions: { [oddId]: { file: '../../ai-provider-secrets.bin' } }
  })
  await body(oddId, [{ role: 'user', content: 'Current version' }])
  await body('orphan', [{ role: 'user', content: 'orphan text' }])
  await put('chat-messages.json', {
    messagesBySid: {
      [oddId]: [{ role: 'user', content: 'Stale duplicate' }],
      legacy: [
        { role: 'assistant', outcome: 'error', content: 'Failed' },
        { role: 'assistant', outcome: 'stopped', content: 'Stopped' }
      ],
      orphan: [{ role: 'user', content: 'orphan legacy text' }]
    }
  })
  const before = await readFile(join(historyDir, 'chat-messages.json'))
  expect(await exportVisibleChatHistory({ historyDir, destination })).toMatchObject({
    success: true,
    count: 3
  })
  const exported = JSON.parse(await readFile(destination, 'utf8'))
  expect(exported.chats).toEqual([
    { ...chat(oddId), messages: [{ role: 'user', text: 'Current version', status: 'completed' }] },
    {
      ...chat('legacy'),
      messages: [
        { role: 'assistant', text: 'Failed', status: 'failed' },
        { role: 'assistant', text: 'Stopped', status: 'cancelled' }
      ]
    },
    { ...chat('empty'), messages: [] }
  ])
  expect(await readFile(join(historyDir, 'chat-messages.json'))).toEqual(before)
  expect(await readdir(historyDir)).not.toContain('chat-messages.json.migrated')
})

it('embeds supported structured and Markdown previews with canonical Base64 and a SHA-256', async () => {
  const gif = Buffer.from('R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7', 'base64')
  const webp = Buffer.from('UklGRiIAAABXRUJQVlA4IBYAAAAwAQCdASoBAAEADsD+JaQAA3AAAAAA', 'base64')
  const jpeg = Buffer.from('ffd8ffe000104a46494600010100000100010000ffd9', 'hex')
  await put('chat-sessions.json', { sessions: [chat('fixture')] })
  await body('fixture', [
    {
      role: 'user',
      content: [
        { type: 'text', text: 'Preview this video' },
        imagePart(dataUrl('image/png', png)),
        imagePart(`DATA:IMAGE/JPEG;BASE64,${jpeg.toString('base64')}`),
        { type: 'file_url', file_url: { url: dataUrl('image/png', png), mimeType: 'video/mp4' } }
      ]
    },
    {
      role: 'assistant',
      content: `Local previews ![GIF](${dataUrl('image/gif', gif)}) ![WebP](${dataUrl('image/webp', webp)})`
    }
  ])
  expect(await exportVisibleChatHistory({ historyDir, destination })).toMatchObject({
    success: true
  })
  const raw = await readFile(destination, 'utf8')
  const document = JSON.parse(raw)
  expect(document.version).toBe(1)
  expect(document.chats[0].messages).toEqual([
    {
      role: 'user',
      text: 'Preview this video',
      status: 'completed',
      attachments: [embedded('image/png', png), embedded('image/jpeg', jpeg), unavailable('video')]
    },
    {
      role: 'assistant',
      text: 'Local previews',
      status: 'completed',
      attachments: [embedded('image/gif', gif), embedded('image/webp', webp)]
    }
  ])
  expect(raw).not.toContain('data:')
})

it('keeps placeholders for mismatched signatures and malformed or unsupported data URLs', async () => {
  const base64 = png.toString('base64')
  const candidates = [
    dataUrl('image/jpeg', png),
    dataUrl('image/svg+xml', png),
    'data:image/png;base64,',
    `data:image/png;base64,${base64.slice(0, -1)}`,
    `data:image/png;base64,${base64.slice(0, -2)}J=`, // Non-zero padding bits
    `data:image/png;base64,${base64.slice(0, 4)} ${base64.slice(4)}`,
    `data:image/png;charset=UTF-8;base64,${base64}`,
    `data:image/png;base64,${base64}#fragment`,
    dataUrl('image/webp', Buffer.from('RIFF0000WAVE'))
  ]
  await put('chat-sessions.json', { sessions: [chat('fixture')] })
  await body('fixture', [
    { role: 'user', content: candidates.map(imagePart) },
    { role: 'assistant', content: `![wrong type](${dataUrl('image/gif', png)})` }
  ])
  expect(await exportVisibleChatHistory({ historyDir, destination })).toMatchObject({
    success: true
  })
  const raw = await readFile(destination, 'utf8')
  const messages = JSON.parse(raw).chats[0].messages
  expect(messages[0].attachments).toEqual(candidates.map(() => unavailable('image')))
  expect(messages[1].attachments).toEqual([unavailable('image')])
  expect(raw).not.toContain(base64)
})

it('shares the decoded byte budget across chats and counts repeated previews', async () => {
  const limit = 2 * 1024 * 1024
  const atLimit = Buffer.concat([png, Buffer.alloc(limit - png.length)])
  const aboveLimit = Buffer.concat([atLimit, Buffer.alloc(1)])
  const preview = imagePart(dataUrl('image/png', atLimit))
  await put('chat-sessions.json', { sessions: [chat('first'), chat('second')] })
  await body('first', [
    { role: 'user', content: [imagePart(dataUrl('image/png', aboveLimit)), preview, preview] }
  ])
  await body('second', [
    { role: 'user', content: [preview, preview, imagePart(dataUrl('image/png', png))] }
  ])
  expect(await exportVisibleChatHistory({ historyDir, destination })).toMatchObject({
    success: true,
    count: 2
  })
  const raw = await readFile(destination, 'utf8')
  const chats = JSON.parse(raw).chats
  expect(
    chats.map(
      ({ messages }: { messages: Array<{ attachments: Array<{ availability: string }> }> }) =>
        messages[0].attachments.map(({ availability }) => availability)
    )
  ).toEqual([
    ['unavailable', 'embedded', 'embedded'],
    ['embedded', 'embedded', 'unavailable']
  ])
  expect(Buffer.byteLength(raw)).toBeLessThan(16 * 1024 * 1024)
})

it('keeps the final manifest within its existing byte limit', async () => {
  const bytes = Buffer.concat([png, Buffer.alloc(2 * 1024 * 1024 - png.length)])
  const preview = imagePart(dataUrl('image/png', bytes))
  await put('chat-sessions.json', { sessions: [chat('fixture')] })
  await body('fixture', [
    {
      role: 'user',
      content: [
        { type: 'text', text: 'x'.repeat(6 * 1024 * 1024) },
        preview,
        preview,
        preview,
        preview
      ]
    }
  ])
  expect(await exportVisibleChatHistory({ historyDir, destination })).toEqual({
    success: false,
    code: 'chat_history_export_invalid'
  })
  expect(await readdir(root)).not.toContain('export.json')
})

it('strips legacy inline private blocks and keeps the surrounding visible text', async () => {
  await put('chat-sessions.json', { sessions: [chat('fixture')] })
  await body('fixture', [
    {
      role: 'assistant',
      content: [
        'Before',
        '<think>\n\nhidden-thought-paragraph\n\n</think>',
        '<tool_result>\n\nhidden-tool-body\n\n</tool_result>',
        '[[uebox-media {"kind":"audio","key":"hidden-storage-key","filePath":"/private/audio"}]]',
        'After'
      ].join('\n\n')
    }
  ])
  expect(await exportVisibleChatHistory({ historyDir, destination })).toMatchObject({
    success: true
  })
  const raw = await readFile(destination, 'utf8')
  expect(JSON.parse(raw).chats[0].messages).toEqual([
    {
      role: 'assistant',
      text: 'Before\n\nAfter',
      status: 'completed',
      attachments: [unavailable('audio')]
    }
  ])
  expect(raw).not.toMatch(/hidden-|\/private/)
})

it('rejects a mismatched body and never publishes part of an export', async () => {
  await put('chat-sessions.json', { sessions: [chat('good'), chat('bad')] })
  await body('good', [{ role: 'user', content: 'first' }])
  await put(join('sessions', fileNameFor('bad')), { sid: 'different-chat', messages: [] })
  expect(await exportVisibleChatHistory({ historyDir, destination })).toEqual({
    success: false,
    code: 'chat_history_export_invalid'
  })
  expect(await readdir(root)).toEqual(['chat-history'])
})

it('rejects an unknown source version or damaged UTF-8 before writing', async () => {
  await put('chat-sessions.json', { version: 99, sessions: [chat('fixture')] })
  const args = { historyDir, destination }
  expect(await exportVisibleChatHistory(args)).toEqual({
    success: false,
    code: 'chat_history_export_invalid'
  })
  await writeFile(join(historyDir, 'chat-sessions.json'), Buffer.from([0xc0, 0xaf]))
  expect(await exportVisibleChatHistory(args)).toEqual({
    success: false,
    code: 'chat_history_export_invalid'
  })
  expect(await readdir(root)).toEqual(['chat-history'])
})

it.skipIf(process.platform === 'win32')('rejects links out of the history directory', async () => {
  await put('chat-sessions.json', { sessions: [chat('fixture')] })
  const outside = join(root, 'outside.json')
  await writeFile(outside, JSON.stringify({ sid: 'fixture', messages: [] }))
  await symlink(outside, join(historyDir, 'sessions', fileNameFor('fixture')))
  expect(await exportVisibleChatHistory({ historyDir, destination })).toEqual({
    success: false,
    code: 'chat_history_export_invalid'
  })
  expect(await readdir(root)).not.toContain('export.json')
})

it('rejects destinations in the source tree and never replaces an existing file', async () => {
  await put('chat-sessions.json', { sessions: [] })
  expect(
    await exportVisibleChatHistory({ historyDir, destination: join(historyDir, 'new.json') })
  ).toEqual({ success: false, code: 'chat_history_export_invalid' })
  expect(await readdir(historyDir)).not.toContain('new.json')
  await writeFile(destination, 'existing contents')
  expect(await exportVisibleChatHistory({ historyDir, destination })).toEqual({
    success: false,
    code: 'chat_history_export_exists'
  })
  expect(await readFile(destination, 'utf8')).toBe('existing contents')
  expect((await readdir(root)).sort()).toEqual(['chat-history', 'export.json'])
})

it('concurrent exports to the same new destination publish one complete document', async () => {
  await put('chat-sessions.json', { sessions: [chat('fixture')] })
  await body('fixture', [{ role: 'user', content: 'complete message' }])
  const args = { historyDir, destination }
  const results = await Promise.all([
    exportVisibleChatHistory(args),
    exportVisibleChatHistory(args)
  ])
  expect(results.map((response) => response.code).sort()).toEqual([
    'chat_history_export_exists',
    'chat_history_exported'
  ])
  expect(JSON.parse(await readFile(destination, 'utf8')).chats[0].messages[0].text).toBe(
    'complete message'
  )
  expect((await readdir(root)).sort()).toEqual(['chat-history', 'export.json'])
})
