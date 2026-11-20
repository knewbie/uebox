import { invoke } from '@tauri-apps/api/core'
import { z } from 'zod'

export const LEGACY_PAGE_SIZE = 20
export const MAX_MANIFEST_BYTES = 16 * 1024 * 1024

const MAX_DATE_SECONDS = 8_640_000_000_000
const timestamp = z.number().int().min(-MAX_DATE_SECONDS).max(MAX_DATE_SECONDS)
const count = z.number().int().min(0).max(Number.MAX_SAFE_INTEGER)
const summarySchema = z.object({
  id: z.string().min(1),
  title: z.string(),
  createdAt: timestamp,
  updatedAt: timestamp
})
const attachmentSchema = z.object({
  kind: z.string(),
  availability: z.enum(['unavailable', 'embedded']),
  id: z.string().optional(),
  mediaType: z.string().optional()
})
const chatSchema = summarySchema.extend({
  messages: z.array(
    z.object({
      role: z.enum(['user', 'assistant']),
      text: z.string(),
      status: z.enum(['completed', 'failed', 'cancelled', 'interrupted']),
      attachments: z.array(attachmentSchema).optional()
    })
  )
})
const pageSchema = z.object({ chats: z.array(summarySchema), nextOffset: count.nullable() })
const importedSchema = z.object({
  inserted: count,
  alreadyImported: count,
  embeddedAttachments: count,
  unavailableAttachments: count
})
const imageType = z.enum(['image/png', 'image/jpeg', 'image/webp', 'image/gif'])
const imageSchema = z.object({
  mediaType: imageType,
  // The complete import is limited to 16 MiB, so an embedded base64 field fits that limit too.
  base64: z
    .string()
    .min(1)
    .max(MAX_MANIFEST_BYTES)
    .refine((value) => value.length % 4 === 0 && /^[A-Za-z0-9+/]+={0,2}$/.test(value))
})
const IMAGE_PREFIXES = {
  'image/png': 'data:image/png;base64,',
  'image/jpeg': 'data:image/jpeg;base64,',
  'image/webp': 'data:image/webp;base64,',
  'image/gif': 'data:image/gif;base64,'
} as const

export type LegacyChatSummary = z.infer<typeof summarySchema>
export type LegacyChat = z.infer<typeof chatSchema>
export type LegacyAttachment = z.infer<typeof attachmentSchema>
export type LegacyImportSummary = z.infer<typeof importedSchema>
export type PreviewableImage = LegacyAttachment & {
  id: string
  mediaType: z.infer<typeof imageType>
}

async function request<T>(
  command: string,
  args: Record<string, unknown>,
  parse: (value: unknown) => T
): Promise<T> {
  try {
    return parse(await invoke<unknown>(command, args))
  } catch {
    // Native errors and schema diagnostics can contain archive text, paths or encoded images.
    throw new Error('legacy_history_unavailable')
  }
}

export function importLegacyChatHistory(manifest: string): Promise<LegacyImportSummary> {
  return request('import_legacy_chat_history', { manifest }, (value) => importedSchema.parse(value))
}

export function listLegacyChats(startOffset: number): Promise<z.infer<typeof pageSchema>> {
  return request('list_legacy_chats', { limit: LEGACY_PAGE_SIZE, offset: startOffset }, (value) => {
    const page = pageSchema.parse(value)
    if (page.nextOffset !== null && page.nextOffset <= startOffset) {
      throw new Error('legacy_history_unavailable')
    }
    return page
  })
}

export function getLegacyChat(chatId: string): Promise<LegacyChat> {
  return request('get_legacy_chat', { chatId }, (value) => {
    const chat = chatSchema.parse(value)
    if (chat.id !== chatId) throw new Error('legacy_history_unavailable')
    return chat
  })
}

export function canPreviewImage(attachment: LegacyAttachment): attachment is PreviewableImage {
  return (
    attachment.kind === 'image' &&
    attachment.availability === 'embedded' &&
    typeof attachment.id === 'string' &&
    attachment.id.length > 0 &&
    imageType.safeParse(attachment.mediaType).success
  )
}

export function readLegacyChatImage(attachment: PreviewableImage): Promise<string> {
  return request('read_legacy_chat_attachment', { attachmentId: attachment.id }, (value) => {
    const image = imageSchema.parse(value)
    if (image.mediaType !== attachment.mediaType) throw new Error('legacy_history_unavailable')
    // Never interpolate a host-controlled MIME value into a data URL.
    return IMAGE_PREFIXES[image.mediaType] + image.base64
  })
}
