import { Buffer } from 'node:buffer'
import { createHash } from 'node:crypto'
import { constants, type BigIntStats } from 'node:fs'
import { link, lstat, mkdtemp, open, realpath, rm } from 'node:fs/promises'
import { basename, dirname, extname, isAbsolute, join, relative, sep } from 'node:path'
import MarkdownIt from 'markdown-it'
import { fileNameFor } from './chatSessionFiles'

const FORMAT = 'uebox-visible-chat-history'
const VERSION = 1
const MAX_SOURCE_FILE_BYTES = 32 * 1024 * 1024
const MAX_SOURCE_BYTES = 128 * 1024 * 1024
const MAX_EXPORT_BYTES = 16 * 1024 * 1024
const MAX_IMAGE_BYTES = 2 * 1024 * 1024
const MAX_ATTACHMENT_BYTES = 8 * 1024 * 1024
const MAX_CHATS = 4096
const MAX_MESSAGES = 100_000

type AttachmentKind = 'image' | 'file' | 'excel' | 'document' | 'video' | 'audio'
type MessageStatus = 'completed' | 'failed' | 'cancelled' | 'interrupted'
type ImageMediaType = 'image/png' | 'image/jpeg' | 'image/webp' | 'image/gif'
type Attachment =
  | { kind: AttachmentKind; availability: 'unavailable' }
  | {
      kind: 'image'
      availability: 'embedded'
      mediaType: ImageMediaType
      sha256: string
      base64: string
    }
interface VisibleMessage {
  role: 'user' | 'assistant'
  text: string
  status: MessageStatus
  attachments?: Attachment[]
}
interface ChatMetadata {
  id: string
  title: string
  createdAt: number
  updatedAt: number
}

export type VisibleChatHistoryExportResult =
  | { success: true; code: 'chat_history_exported'; count: number }
  | {
      success: false
      code:
        | 'chat_history_export_invalid'
        | 'chat_history_export_unavailable'
        | 'chat_history_export_exists'
        | 'chat_history_export_write_failed'
    }

interface ReadBudget {
  bytes: number
  messages: number
  attachmentBytes: number
}
class InvalidHistory extends Error {}

function hasCode(error: unknown, code: string): boolean {
  return error instanceof Error && 'code' in error && error.code === code
}

function object(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined
}

function envelope(value: unknown): Record<string, unknown> {
  const record = object(value)
  // The on-disk Electron bodies are unversioned. Do not guess future formats.
  if (!record || (record.version !== undefined && record.version !== VERSION))
    throw new InvalidHistory()
  return record
}

function localAbsolute(path: string): boolean {
  return isAbsolute(path) && !path.includes('\0')
}

async function sourceDirectory(path: string): Promise<boolean> {
  try {
    const info = await lstat(path)
    if (!info.isDirectory()) throw new InvalidHistory()
    return true
  } catch (error) {
    if (hasCode(error, 'ENOENT')) return false
    throw error
  }
}

/** Read a bounded, committed source file. Never follow an index-supplied filename. */
async function readSource(path: string, budget: ReadBudget): Promise<string | undefined> {
  let entry: BigIntStats
  try {
    entry = await lstat(path, { bigint: true })
  } catch (error) {
    if (hasCode(error, 'ENOENT')) return undefined
    throw error
  }
  const limit = Math.min(budget.bytes, MAX_SOURCE_FILE_BYTES)
  if (!entry.isFile() || entry.nlink !== 1n || entry.size > BigInt(limit))
    throw new InvalidHistory()
  const flags =
    process.platform === 'win32'
      ? constants.O_RDONLY
      : constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK
  const file = await open(path, flags)
  try {
    const opened = await file.stat({ bigint: true })
    if (
      !opened.isFile() ||
      opened.nlink !== 1n ||
      opened.dev !== entry.dev ||
      opened.ino !== entry.ino ||
      opened.size !== entry.size ||
      opened.mtimeNs !== entry.mtimeNs
    )
      throw new InvalidHistory()
    // A writer growing the file after stat can only make us read one extra byte.
    const bytes = Buffer.alloc(Number(opened.size) + 1)
    let length = 0
    while (length < bytes.length) {
      const { bytesRead } = await file.read(bytes, length, bytes.length - length, null)
      if (bytesRead === 0) break
      length += bytesRead
    }
    if (
      BigInt(length) !== opened.size ||
      (await file.stat({ bigint: true })).mtimeNs !== opened.mtimeNs
    )
      throw new InvalidHistory()
    budget.bytes -= length
    try {
      return new TextDecoder('utf-8', { fatal: true }).decode(bytes.subarray(0, length))
    } catch {
      throw new InvalidHistory()
    }
  } finally {
    await file.close()
  }
}

function parse(text: string): unknown {
  try {
    return JSON.parse(text) as unknown
  } catch {
    throw new InvalidHistory()
  }
}

const markdown = new MarkdownIt({ html: true, linkify: true })
// Parse unsafe destinations as tokens too so they never fall back to visible text.
markdown.validateLink = () => true

const PNG_SIGNATURE = Buffer.from('89504e470d0a1a0a', 'hex')
const JPEG_SIGNATURE = Buffer.from('ffd8ff', 'hex')
const GIF87_SIGNATURE = Buffer.from('GIF87a')
const GIF89_SIGNATURE = Buffer.from('GIF89a')
const RIFF_SIGNATURE = Buffer.from('RIFF')
const WEBP_SIGNATURE = Buffer.from('WEBP')

function matchesSignature(image: Buffer, mediaType: ImageMediaType): boolean {
  switch (mediaType) {
    case 'image/png':
      return image.subarray(0, 8).equals(PNG_SIGNATURE)
    case 'image/jpeg':
      return image.subarray(0, 3).equals(JPEG_SIGNATURE)
    case 'image/gif':
      return (
        image.subarray(0, 6).equals(GIF87_SIGNATURE) || image.subarray(0, 6).equals(GIF89_SIGNATURE)
      )
    case 'image/webp':
      return (
        image.subarray(0, 4).equals(RIFF_SIGNATURE) && image.subarray(8, 12).equals(WEBP_SIGNATURE)
      )
  }
}

function unavailable(kind: AttachmentKind): Attachment {
  return { kind, availability: 'unavailable' }
}

function imageAttachment(url: unknown, budget?: ReadBudget): Attachment {
  if (!budget || typeof url !== 'string') return unavailable('image')
  const limit = Math.min(budget.attachmentBytes, MAX_IMAGE_BYTES)
  if (limit === 0) return unavailable('image')
  const prefix = /^data:(image\/(?:png|jpeg|webp|gif));base64,/i.exec(url)
  if (!prefix) return unavailable('image')

  const length = url.length - prefix[0].length
  if (length === 0 || length % 4 !== 0 || length > 4 * Math.ceil(limit / 3))
    return unavailable('image')
  const base64 = url.slice(prefix[0].length)
  if (!/^[A-Za-z0-9+/]+={0,2}$/.test(base64)) return unavailable('image')
  const padding = base64.endsWith('==') ? 2 : base64.endsWith('=') ? 1 : 0
  const decodedLength = (length / 4) * 3 - padding
  if (decodedLength > limit) return unavailable('image')

  const mediaType = prefix[1].toLowerCase() as ImageMediaType
  const image = Buffer.from(base64, 'base64')
  if (
    image.length !== decodedLength ||
    image.toString('base64') !== base64 ||
    !matchesSignature(image, mediaType)
  )
    return unavailable('image')
  budget.attachmentBytes -= image.length
  return {
    kind: 'image',
    availability: 'embedded',
    mediaType,
    sha256: createHash('sha256').update(image).digest('hex'),
    base64
  }
}

function stripPrivateBlocks(value: string, attachments: Attachment[]): string {
  return value
    .replace(
      /<(think|reasoning|tool_(?:call|result)|attachments)\b[^<>]{0,512}>[\s\S]*?(?:<\/\1\s*>|$)/gi,
      ''
    )
    .replace(/^[ \t]*\[\[uebox-media ([^\r\n]*)\]\][ \t]*$/gm, (_line: string, data: string) => {
      let kind: AttachmentKind = 'file'
      try {
        const type = object(JSON.parse(data))?.kind
        if (type === 'audio' || type === 'video' || type === 'image') kind = type
      } catch {
        // A malformed legacy reference still has no portable destination.
      }
      attachments.push(unavailable(kind))
      return ''
    })
}

function visibleText(value: string, attachments: Attachment[], budget?: ReadBudget): string {
  const paragraphs: string[] = []
  for (const block of markdown.parse(stripPrivateBlocks(value, attachments), {})) {
    if (block.type === 'fence' || block.type === 'code_block') {
      paragraphs.push(block.content.trimEnd())
    } else if (block.type === 'html_block') {
      if (/^\s*<img\b/i.test(block.content)) attachments.push(unavailable('image'))
    } else if (block.type === 'inline' && block.children) {
      let text = ''
      let autoLink = false
      for (const token of block.children) {
        switch (token.type) {
          case 'html_inline':
            if (/^<img\b/i.test(token.content)) attachments.push(unavailable('image'))
            break
          case 'link_open':
            autoLink = token.markup === 'linkify' || token.markup === 'autolink'
            break
          case 'link_close':
            autoLink = false
            break
          case 'image':
            attachments.push(imageAttachment(token.attrGet('src'), budget))
            break
          case 'text':
          case 'code_inline':
            if (!autoLink) text += token.content
            break
          case 'softbreak':
          case 'hardbreak':
            text += '\n'
        }
      }
      if (text.trim()) paragraphs.push(text.trim())
    }
  }
  return paragraphs.join('\n\n')
}

function fileKind(mimeType: unknown): AttachmentKind {
  if (typeof mimeType !== 'string') return 'file'
  if (mimeType.startsWith('video/')) return 'video'
  if (mimeType.startsWith('audio/')) return 'audio'
  if (mimeType.startsWith('image/')) return 'image'
  return 'file'
}

function visibleMessage(value: unknown, budget: ReadBudget): VisibleMessage | undefined {
  const message = object(value)
  if (!message || (message.role !== 'user' && message.role !== 'assistant')) return undefined
  let status: MessageStatus = 'completed'
  if (message.role === 'assistant') {
    if (message.status === 'typing') status = 'interrupted'
    else if (message.outcome === 'error') status = 'failed'
    else if (message.outcome === 'stopped') status = 'cancelled'
  }
  const attachments: Attachment[] = []
  const texts: string[] = []
  const content = message.content
  if (typeof content === 'string') {
    // Old placeholder strings were persisted as content in the language active at the time.
    if (status !== 'interrupted' || !/^(?:正在思考|Thinking)(?:\.{3}|…)?$/i.test(content.trim()))
      texts.push(visibleText(content, attachments, budget))
  } else if (Array.isArray(content)) {
    for (const item of content) {
      const part = object(item)
      if (part?.type === 'text' && typeof part.text === 'string')
        texts.push(visibleText(part.text, attachments, budget))
      else if (part?.type === 'image_url')
        attachments.push(imageAttachment(object(part.image_url)?.url, budget))
      else if (part?.type === 'file_url')
        attachments.push(unavailable(fileKind(object(part.file_url)?.mimeType)))
    }
  }
  if (Array.isArray(message.excelFiles)) {
    for (const item of message.excelFiles) {
      const attachment = object(item)
      if (typeof attachment?.fileName !== 'string') continue
      const { kind } = attachment
      attachments.push(
        unavailable(kind === 'document' || kind === 'video' || kind === 'audio' ? kind : 'excel')
      )
    }
  }
  const text = texts.filter(Boolean).join('\n\n')
  if (!text && attachments.length === 0 && status === 'completed') return undefined
  return {
    role: message.role,
    text,
    status,
    ...(attachments.length ? { attachments } : {})
  }
}

function validTime(value: unknown): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0
}

function readMetadata(raw: string | undefined): ChatMetadata[] {
  if (!raw) return []
  const { sessions } = envelope(parse(raw))
  if (!Array.isArray(sessions) || sessions.length > MAX_CHATS) throw new InvalidHistory()
  const ids = new Set<string>()
  return sessions.map((item) => {
    const metadata = object(item)
    if (!metadata) throw new InvalidHistory()
    const { id, title, createdAt, updatedAt } = metadata
    if (
      typeof id !== 'string' ||
      !id.trim() ||
      id.length > 256 ||
      id.includes('\0') ||
      ids.has(id) ||
      typeof title !== 'string' ||
      !validTime(createdAt) ||
      !validTime(updatedAt)
    )
      throw new InvalidHistory()
    ids.add(id)
    return { id, title: visibleText(title, []), createdAt, updatedAt }
  })
}

async function buildExport(historyDir: string): Promise<{ content: string; count: number }> {
  const budget: ReadBudget = {
    bytes: MAX_SOURCE_BYTES,
    messages: MAX_MESSAGES,
    attachmentBytes: MAX_ATTACHMENT_BYTES
  }
  const metadata = readMetadata(await readSource(join(historyDir, 'chat-sessions.json'), budget))
  const sessionsDir = join(historyDir, 'sessions')
  const hasSessions = metadata.length > 0 && (await sourceDirectory(sessionsDir))
  let legacy: Record<string, unknown> | undefined
  const prefix = `{"format":"${FORMAT}","version":${VERSION},"chats":[`
  let bytes = Buffer.byteLength(prefix) + 3 // Closing brackets and newline
  const encoded: string[] = []
  for (const chat of metadata) {
    const body = hasSessions
      ? await readSource(join(sessionsDir, fileNameFor(chat.id)), budget)
      : undefined
    let rows: unknown
    if (body !== undefined) {
      const session = envelope(parse(body))
      if (session.sid !== chat.id) throw new InvalidHistory()
      rows = session.messages
    } else {
      if (!legacy) {
        const raw = await readSource(join(historyDir, 'chat-messages.json'), budget)
        const map = raw === undefined ? undefined : envelope(parse(raw)).messagesBySid
        legacy = object(map)
        if (!legacy && map !== undefined) throw new InvalidHistory()
        legacy ??= {}
      }
      rows = Object.hasOwn(legacy, chat.id) ? legacy[chat.id] : []
    }
    if (!Array.isArray(rows) || rows.length > budget.messages) throw new InvalidHistory()
    budget.messages -= rows.length
    const messages = rows.flatMap((row) => {
      const message = visibleMessage(row, budget)
      return message ? [message] : []
    })
    const json = JSON.stringify({ ...chat, messages })
    bytes += Buffer.byteLength(json) + (encoded.length ? 1 : 0)
    if (bytes > MAX_EXPORT_BYTES) throw new InvalidHistory()
    encoded.push(json)
  }
  return { content: `${prefix}${encoded.join(',')}]}\n`, count: metadata.length }
}

function inSourceTree(source: string, parent: string): boolean {
  const subpath = relative(source, parent)
  return !subpath || (!isAbsolute(subpath) && subpath !== '..' && !subpath.startsWith(`..${sep}`))
}

async function publish(parent: string, destination: string, content: string): Promise<void> {
  const staging = await mkdtemp(join(parent, '.uebox-chat-export-'))
  const stagedFile = join(staging, 'export.json')
  try {
    const file = await open(stagedFile, 'wx', 0o600)
    try {
      await file.writeFile(content, 'utf8')
      await file.sync()
    } finally {
      await file.close()
    }
    await link(stagedFile, destination)
  } finally {
    await rm(staging, { recursive: true, force: true }).catch(() => undefined)
  }
}

/** Call only after an explicit Save dialog. Both paths are supplied by the main process. */
export async function exportVisibleChatHistory({
  historyDir,
  destination
}: {
  historyDir: string
  destination: string
}): Promise<VisibleChatHistoryExportResult> {
  if (
    !localAbsolute(historyDir) ||
    !localAbsolute(destination) ||
    extname(destination).toLowerCase() !== '.json'
  )
    return { success: false, code: 'chat_history_export_invalid' }

  let source: string
  let document: Awaited<ReturnType<typeof buildExport>>
  try {
    if (!(await sourceDirectory(historyDir)))
      return { success: false, code: 'chat_history_export_unavailable' }
    source = await realpath(historyDir)
    document = await buildExport(source)
  } catch (error) {
    return {
      success: false,
      code:
        error instanceof InvalidHistory
          ? 'chat_history_export_invalid'
          : 'chat_history_export_unavailable'
    }
  }

  try {
    const parent = await realpath(dirname(destination))
    if (inSourceTree(source, parent)) return { success: false, code: 'chat_history_export_invalid' }
    await publish(parent, join(parent, basename(destination)), document.content)
    return { success: true, code: 'chat_history_exported', count: document.count }
  } catch (error) {
    return {
      success: false,
      code: hasCode(error, 'EEXIST')
        ? 'chat_history_export_exists'
        : 'chat_history_export_write_failed'
    }
  }
}
