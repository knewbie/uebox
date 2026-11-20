import { invoke } from '@tauri-apps/api/core'
import { z } from 'zod'

export const HISTORY_PAGE_SIZE = 20

// JavaScript Date supports this range in Unix seconds; the host sends seconds, not milliseconds.
const MAX_DATE_SECONDS = 8_640_000_000_000
const timestamp = z.number().int().min(-MAX_DATE_SECONDS).max(MAX_DATE_SECONDS)
const chatId = z.string().min(1)
const offset = z.number().int().min(0).max(Number.MAX_SAFE_INTEGER)
const summarySchema = z.object({
  id: chatId,
  engine: z.enum(['unreal', 'unity', 'godot']),
  title: z.string(),
  createdAt: timestamp,
  updatedAt: timestamp
})
const actorReceiptSchema = z.object({
  approvalId: z.string(),
  status: z.enum(['applied', 'mismatch', 'not_executed', 'unknown']),
  actualLabel: z.string().nullable()
})
const nameReceiptSchema = actorReceiptSchema.omit({ actualLabel: true }).extend({
  actualName: z.string().nullable()
})
const turnSchema = z.object({
  // Keep IDs as decimal strings, including values outside JavaScript's exact integer range.
  id: z.string().regex(/^\d+$/),
  userText: z.string(),
  assistantText: z.string().nullable(),
  status: z.enum(['pending', 'completed', 'failed', 'cancelled', 'interrupted']),
  createdAt: timestamp,
  finishedAt: timestamp.nullable(),
  actorLabelReceipts: z.array(actorReceiptSchema).optional(),
  nodeNameReceipts: z.array(nameReceiptSchema).optional()
})
const pageSchema = z.object({
  chats: z.array(summarySchema),
  nextOffset: offset.nullable()
})
const chatSchema = summarySchema.extend({ turns: z.array(turnSchema) })

export type AgentChatSummary = z.infer<typeof summarySchema>
export type AgentChatTurn = z.infer<typeof turnSchema>
export type ActorLabelReceipt = z.infer<typeof actorReceiptSchema>
export type AgentChatPage = z.infer<typeof pageSchema>
export type AgentChat = z.infer<typeof chatSchema>

export async function listAgentChats(startOffset: number): Promise<AgentChatPage> {
  try {
    const page = pageSchema.parse(
      await invoke<unknown>('list_agent_chats', {
        limit: HISTORY_PAGE_SIZE,
        offset: offset.parse(startOffset)
      })
    )
    if (page.nextOffset !== null && page.nextOffset <= startOffset) {
      throw new Error('history_unavailable')
    }
    return page
  } catch {
    // Native errors and schema diagnostics can contain private history. Never expose them.
    throw new Error('history_unavailable')
  }
}

export async function getAgentChat(id: string): Promise<AgentChat> {
  try {
    const chat = chatSchema.parse(
      await invoke<unknown>('get_agent_chat', { chatId: chatId.parse(id) })
    )
    if (chat.id !== id || new Set(chat.turns.map((turn) => turn.id)).size !== chat.turns.length) {
      throw new Error('history_unavailable')
    }
    return chat
  } catch {
    throw new Error('history_unavailable')
  }
}
