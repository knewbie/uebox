import { invoke } from '@tauri-apps/api/core'
import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { nextTick } from 'vue'
import { createI18n } from 'vue-i18n'
import AgentHistoryPanel from './AgentHistoryPanel.vue'
import { getAgentChat, listAgentChats, type AgentChat, type AgentChatSummary } from './commands'
import { historyTestMessages } from './messages.fixture'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))

const ipc = vi.mocked(invoke)
const summary: AgentChatSummary = {
  id: 'chat-a',
  engine: 'unreal',
  title: 'Saved chat A',
  createdAt: 1_700_000_000,
  updatedAt: 1_700_000_060
}
const other: AgentChatSummary = {
  ...summary,
  id: 'chat-b',
  engine: 'godot',
  title: 'Saved chat B'
}
const third: AgentChatSummary = {
  ...summary,
  id: 'chat-c',
  engine: 'unity',
  title: 'Chat C'
}
const detail: AgentChat = {
  ...summary,
  turns: [
    {
      id: '9007199254740993',
      userText: 'Saved question',
      assistantText: 'Saved answer',
      status: 'completed',
      createdAt: summary.createdAt,
      finishedAt: summary.updatedAt
    }
  ]
}
let panel: VueWrapper | undefined

beforeEach(() => ipc.mockReset())
afterEach(() => {
  panel?.unmount()
  panel = undefined
})

function mountPanel(locale: 'en-US' | 'zh-CN' = 'en-US'): VueWrapper {
  const i18n = createI18n({ legacy: false, locale, messages: historyTestMessages })
  panel = mount(AgentHistoryPanel, { global: { plugins: [i18n] } })
  return panel
}

async function openPanel(): Promise<VueWrapper> {
  const wrapper = mountPanel()
  await wrapper.get('[data-history-toggle]').trigger('click')
  await flushPromises()
  return wrapper
}

async function selectChat(wrapper: VueWrapper, id = summary.id): Promise<void> {
  await wrapper.get(`[data-history-chat="${id}"]`).trigger('click')
  await flushPromises()
}

function deferredReply(): {
  promise: Promise<unknown>
  resolve: (value: unknown) => void
  reject: (reason: unknown) => void
} {
  let resolve!: (value: unknown) => void
  let reject!: (reason: unknown) => void
  const promise = new Promise((done, fail) => {
    resolve = done
    reject = fail
  })
  return { promise, resolve, reject }
}

describe('explicit read-only history panel', () => {
  it.each(['en-US', 'zh-CN'] as const)('reads only after opening (%s)', async (locale) => {
    ipc.mockResolvedValueOnce({ chats: [], nextOffset: null })
    const wrapper = mountPanel(locale)
    const text = historyTestMessages[locale].tauriPreview.history
    await flushPromises()
    expect(ipc).not.toHaveBeenCalled()
    expect(wrapper.text()).toContain(text.title)
    expect(wrapper.get('[data-history-toggle]').text()).toBe(text.open)
    expect(wrapper.get('[data-history-toggle]').attributes('aria-expanded')).toBe('false')

    await wrapper.get('[data-history-toggle]').trigger('click')
    await flushPromises()
    expect(ipc).toHaveBeenCalledExactlyOnceWith('list_agent_chats', { limit: 20, offset: 0 })
    expect(wrapper.get('[data-history-toggle]').attributes('aria-expanded')).toBe('true')
    expect(wrapper.text()).toContain(text.emptyList)
    expect(wrapper.text()).not.toContain('tauriPreview.history.')
    expect(wrapper.find('input, textarea, form, a').exists()).toBe(false)
  })

  it('paginates with the returned cursor, blocks duplicate pages, and refreshes from zero', async () => {
    const more = deferredReply()
    ipc
      .mockResolvedValueOnce({ chats: [summary], nextOffset: 37 })
      .mockReturnValueOnce(more.promise)
      .mockResolvedValueOnce({ chats: [third], nextOffset: null })
    const wrapper = await openPanel()
    await wrapper.get('[data-history-more]').trigger('click')
    await wrapper.get('[data-history-more]').trigger('click')
    expect(ipc).toHaveBeenCalledTimes(2)
    expect(wrapper.get('[data-history-more]').attributes('disabled')).toBeDefined()
    more.resolve({ chats: [summary, other], nextOffset: null })
    await flushPromises()
    expect(wrapper.findAll('[data-history-chat]')).toHaveLength(2)
    expect(wrapper.find('[data-history-more]').exists()).toBe(false)

    await wrapper.get('[data-history-refresh]').trigger('click')
    await flushPromises()
    expect(ipc.mock.calls).toEqual([
      ['list_agent_chats', { limit: 20, offset: 0 }],
      ['list_agent_chats', { limit: 20, offset: 37 }],
      ['list_agent_chats', { limit: 20, offset: 0 }]
    ])
    expect(wrapper.text()).toContain(third.title)
    expect(wrapper.text()).not.toContain(summary.title)
  })

  it('shows a retryable first-read error without an empty state or raw IPC details', async () => {
    ipc.mockRejectedValueOnce('private /tmp/history.sqlite').mockResolvedValueOnce({
      chats: [],
      nextOffset: null
    })
    const wrapper = await openPanel()
    expect(wrapper.get('[role="alert"]').text()).toContain('Unable to load chat history')
    expect(wrapper.text()).not.toContain('No saved chats')
    expect(wrapper.text()).not.toContain('/tmp/history.sqlite')
    await wrapper.get('[data-history-refresh]').trigger('click')
    await flushPromises()
    expect(wrapper.find('[role="alert"]').exists()).toBe(false)
    expect(wrapper.text()).toContain('No saved chats')
  })

  it('preserves the rows and pagination cursor after a failed page or refresh', async () => {
    ipc
      .mockResolvedValueOnce({ chats: [summary], nextOffset: 20 })
      .mockRejectedValueOnce(new Error('private detail'))
      .mockResolvedValueOnce({ chats: [other], nextOffset: null })
      .mockRejectedValueOnce('private refresh error')
    const wrapper = await openPanel()
    await wrapper.get('[data-history-more]').trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain(summary.title)
    expect(wrapper.find('[data-history-more]').exists()).toBe(true)
    await wrapper.get('[data-history-more]').trigger('click')
    await flushPromises()
    expect(ipc.mock.calls[2]).toEqual(['list_agent_chats', { limit: 20, offset: 20 }])
    expect(wrapper.findAll('[data-history-chat]')).toHaveLength(2)
    expect(wrapper.find('[role="alert"]').exists()).toBe(false)
    await wrapper.get('[data-history-refresh]').trigger('click')
    await flushPromises()
    expect(wrapper.findAll('[data-history-chat]')).toHaveLength(2)
    expect(wrapper.text()).toContain('Unable to load chat history')
    expect(wrapper.text()).not.toContain('private')
  })

  it('renders title and both speakers as plain text with Unix-second dates and stable string IDs', async () => {
    const title = '<img src=x onerror="bad()"> **Title**'
    const userText = '<script>bad()</script>\n[run](javascript:bad())'
    const assistantText = '<b>raw answer</b>\nsecond line'
    ipc
      .mockResolvedValueOnce({ chats: [{ ...summary, title }], nextOffset: null })
      .mockResolvedValueOnce({
        ...detail,
        title,
        turns: [{ ...detail.turns[0], userText, assistantText }]
      })
    const wrapper = await openPanel()
    expect(ipc).toHaveBeenCalledTimes(1)
    expect(wrapper.text()).toContain('Select a chat')
    await selectChat(wrapper)
    expect(ipc.mock.calls[1]).toEqual(['get_agent_chat', { chatId: summary.id }])
    expect(wrapper.get('[data-history-detail] h3').text()).toBe(title)
    expect(wrapper.get('[data-history-user]').element.textContent).toBe(userText)
    expect(wrapper.get('[data-history-assistant]').element.textContent).toBe(assistantText)
    expect(wrapper.get('[data-history-turn]').attributes('data-history-turn')).toBe(
      '9007199254740993'
    )
    const created = wrapper.get('[data-history-detail] [data-history-created] time')
    expect(created.attributes('datetime')).toBe('2023-11-14T22:13:20.000Z')
    expect(created.text()).toBe(new Date(summary.createdAt * 1000).toLocaleString('en-US'))
    expect(wrapper.find('img, script, a, b, input, textarea, form').exists()).toBe(false)
    expect(ipc.mock.calls.map(([command]) => command)).toEqual([
      'list_agent_chats',
      'get_agent_chat'
    ])
  })

  it.each(['en-US', 'zh-CN'] as const)(
    'shows every turn status and nullable fields (%s)',
    async (locale) => {
      const statuses = ['pending', 'completed', 'failed', 'cancelled', 'interrupted'] as const
      ipc
        .mockResolvedValueOnce({ chats: [{ ...summary, title: '' }], nextOffset: null })
        .mockResolvedValueOnce({
          ...detail,
          title: '',
          turns: statuses.map((status, index) => ({
            ...detail.turns[0],
            id: String(index + 1),
            status,
            assistantText: null,
            finishedAt: null
          }))
        })
      const wrapper = mountPanel(locale)
      await wrapper.get('[data-history-toggle]').trigger('click')
      await flushPromises()
      await selectChat(wrapper)
      const text = historyTestMessages[locale].tauriPreview.history
      expect(wrapper.get('[data-history-detail] h3').text()).toBe(text.untitled)
      expect(wrapper.text()).toContain(text.engines.unreal)
      for (const status of statuses) expect(wrapper.text()).toContain(text.statuses[status])
      expect(wrapper.findAll('[data-history-assistant]')).toHaveLength(5)
      expect(wrapper.get('[data-history-assistant]').text()).toBe(text.noAssistantText)
      expect(wrapper.get('[data-history-finished]').text()).toContain(text.unfinished)
      expect(wrapper.find('[data-history-finished] time').exists()).toBe(false)
      expect(wrapper.text()).not.toContain('null')
      expect(wrapper.text()).not.toContain('tauriPreview.history.')
    }
  )

  it('shows an empty chat and retries a failed detail without exposing the error', async () => {
    ipc
      .mockResolvedValueOnce({ chats: [summary], nextOffset: null })
      .mockRejectedValueOnce('private /tmp/detail.sqlite')
      .mockResolvedValueOnce({ ...detail, turns: [] })
    const wrapper = await openPanel()
    await selectChat(wrapper)
    expect(wrapper.text()).toContain('Unable to load this chat')
    expect(wrapper.text()).not.toContain('/tmp/detail.sqlite')
    await wrapper.get('[data-history-retry]').trigger('click')
    await flushPromises()
    expect(ipc.mock.calls[2]).toEqual(['get_agent_chat', { chatId: summary.id }])
    expect(wrapper.text()).toContain('No turns saved')
    expect(wrapper.find('[role="alert"]').exists()).toBe(false)
  })
})

describe('optional Actor label receipts', () => {
  it.each(['en-US', 'zh-CN'] as const)(
    'renders all four outcomes as independent plain text (%s)',
    async (locale) => {
      const statuses = ['applied', 'mismatch', 'not_executed', 'unknown'] as const
      const actualLabel = '<b>Actual label</b>\nsecond line'
      const approvalId = '<img src=x onerror="bad()">'
      const receipts = statuses.map((status, index) => ({
        approvalId: `${approvalId}${index}`,
        status,
        actualLabel: index === 0 ? actualLabel : index === 1 ? '' : null
      }))
      ipc.mockResolvedValueOnce({ chats: [summary], nextOffset: null }).mockResolvedValueOnce({
        ...detail,
        turns: [{ ...detail.turns[0], actorLabelReceipts: receipts }]
      })
      const wrapper = mountPanel(locale)
      await wrapper.get('[data-history-toggle]').trigger('click')
      await flushPromises()
      await selectChat(wrapper)
      const text = historyTestMessages[locale].tauriPreview.history.actorReceipts
      const list = wrapper.get('[data-history-receipts]')
      expect(list.get('h5').text()).toBe(text.title)
      const rows = list.findAll('[data-history-receipt]')
      expect(rows).toHaveLength(4)
      for (const [index, row] of rows.entries()) {
        expect(row.text()).toContain(text.statuses[statuses[index]])
        expect(row.get('[data-history-approval-id]').element.textContent).toBe(
          `${approvalId}${index}`
        )
      }
      expect(rows[0].get('[data-history-actual-label]').element.textContent).toBe(actualLabel)
      expect(rows[1].get('[data-history-actual-label]').element.textContent).toBe('')
      expect(rows[2].get('[data-history-actual-label]').text()).toBe(text.noActualLabel)
      expect(rows[3].get('[data-history-actual-label]').text()).toBe(text.noActualLabel)
      expect(wrapper.get('[data-history-assistant]').text()).toBe('Saved answer')
      expect(wrapper.find('img, b, a, input, textarea, form').exists()).toBe(false)
      expect(wrapper.text()).not.toContain('tauriPreview.history.')
      expect(ipc.mock.calls.map(([command]) => command)).toEqual([
        'list_agent_chats',
        'get_agent_chat'
      ])
    }
  )

  it.each([{ receipts: undefined }, { receipts: [] }])(
    'keeps the original turn when optional receipts are $receipts',
    async ({ receipts }) => {
      ipc.mockResolvedValueOnce({ chats: [summary], nextOffset: null }).mockResolvedValueOnce({
        ...detail,
        turns: [{ ...detail.turns[0], actorLabelReceipts: receipts }]
      })
      const wrapper = await openPanel()
      await selectChat(wrapper)
      expect(wrapper.find('[data-history-receipts]').exists()).toBe(false)
      expect(wrapper.get('[data-history-assistant]').text()).toBe('Saved answer')
      expect(wrapper.text()).not.toContain('Actor label receipts')
    }
  )
})

it.each(['unity', 'godot'] as const)('renders a saved %s name as plain text', async (engine) => {
  ipc
    .mockResolvedValueOnce({ chats: [{ ...summary, engine }], nextOffset: null })
    .mockResolvedValueOnce({
      ...detail,
      engine,
      turns: [
        {
          ...detail.turns[0],
          nodeNameReceipts: [
            {
              approvalId: 'name-change',
              status: 'applied',
              actualName: '<img src=x> Player'
            }
          ]
        }
      ]
    })
  const wrapper = await openPanel()
  await selectChat(wrapper)
  const block = wrapper.get('[data-history-receipts]')
  expect(block.get('h5').text()).toBe('Name change receipts')
  expect(block.get('[data-history-actual-label]').text()).toBe('<img src=x> Player')
  expect(block.find('img').exists()).toBe(false)
})

describe('asynchronous response ordering', () => {
  it.each(['resolve', 'reject'] as const)(
    'ignores an older detail %s after another selection',
    async (settle) => {
      const slow = deferredReply()
      ipc
        .mockResolvedValueOnce({ chats: [summary, other], nextOffset: null })
        .mockReturnValueOnce(slow.promise)
        .mockResolvedValueOnce({ ...other, turns: [] })
      const wrapper = await openPanel()
      await selectChat(wrapper)
      expect(wrapper.text()).toContain('Loading chat…')
      await selectChat(wrapper, other.id)
      if (settle === 'resolve') slow.resolve(detail)
      else slow.reject('private stale detail')
      await flushPromises()
      expect(wrapper.get('[data-history-detail] h3').text()).toBe(other.title)
      expect(wrapper.find('[role="alert"]').exists()).toBe(false)
      expect(wrapper.text()).not.toContain('Saved answer')
      expect(wrapper.text()).not.toContain('private stale detail')
      expect(wrapper.get(`[data-history-chat="${other.id}"]`).attributes('aria-pressed')).toBe(
        'true'
      )
    }
  )

  it.each(['resolve', 'reject'] as const)(
    'ignores an older list %s after a newer refresh',
    async (settle) => {
      const slow = deferredReply()
      const current = deferredReply()
      ipc.mockReturnValueOnce(slow.promise).mockReturnValueOnce(current.promise)
      const wrapper = await openPanel()
      expect(wrapper.text()).toContain('Loading chats')
      expect(wrapper.text()).not.toContain('No saved chats')
      await wrapper.get('[data-history-refresh]').trigger('click')
      current.resolve({ chats: [other], nextOffset: null })
      await flushPromises()
      if (settle === 'resolve') slow.resolve({ chats: [summary], nextOffset: 20 })
      else slow.reject('private stale list')
      await flushPromises()
      expect(wrapper.text()).toContain(other.title)
      expect(wrapper.text()).not.toContain(summary.title)
      expect(wrapper.find('[data-history-more]').exists()).toBe(false)
      expect(wrapper.find('[role="alert"]').exists()).toBe(false)
      expect(wrapper.text()).not.toContain('Loading chats')
    }
  )

  it('keeps the newest loading state when an older request finishes first', async () => {
    const slow = deferredReply()
    const current = deferredReply()
    ipc.mockReturnValueOnce(slow.promise).mockReturnValueOnce(current.promise)
    const wrapper = await openPanel()
    await wrapper.get('[data-history-refresh]').trigger('click')
    slow.resolve({ chats: [summary], nextOffset: null })
    await flushPromises()
    expect(wrapper.text()).toContain('Loading chats')
    expect(wrapper.text()).not.toContain(summary.title)
    current.resolve({ chats: [], nextOffset: null })
    await flushPromises()
    expect(wrapper.text()).not.toContain('Loading chats')
    expect(wrapper.text()).toContain('No saved chats')
  })

  it('ignores a pending page and detail when refresh replaces the list', async () => {
    const more = deferredReply()
    const chat = deferredReply()
    ipc
      .mockResolvedValueOnce({ chats: [summary], nextOffset: 20 })
      .mockReturnValueOnce(chat.promise)
      .mockReturnValueOnce(more.promise)
      .mockResolvedValueOnce({ chats: [third], nextOffset: null })
    const wrapper = await openPanel()
    await selectChat(wrapper)
    await wrapper.get('[data-history-more]').trigger('click')
    await wrapper.get('[data-history-refresh]').trigger('click')
    await flushPromises()
    more.resolve({ chats: [other], nextOffset: 40 })
    chat.resolve(detail)
    await flushPromises()
    expect(wrapper.findAll('[data-history-chat]')).toHaveLength(1)
    expect(wrapper.text()).toContain(third.title)
    expect(wrapper.text()).not.toContain(other.title)
    expect(wrapper.find('[data-history-detail]').exists()).toBe(false)
    expect(wrapper.text()).toContain('Select a chat')
  })

  it('discards pending list responses across closing and reopening', async () => {
    const old = deferredReply()
    ipc.mockReturnValueOnce(old.promise).mockResolvedValueOnce({ chats: [other], nextOffset: null })
    const wrapper = await openPanel()
    await wrapper.get('[data-history-toggle]').trigger('click')
    expect(wrapper.find('[data-history-content]').exists()).toBe(false)
    await wrapper.get('[data-history-toggle]').trigger('click')
    await flushPromises()
    old.resolve({ chats: [summary], nextOffset: 20 })
    await flushPromises()
    expect(wrapper.text()).toContain(other.title)
    expect(wrapper.text()).not.toContain(summary.title)
    expect(ipc).toHaveBeenCalledTimes(2)
  })

  it('discards a detail response across closing and reopening', async () => {
    const old = deferredReply()
    ipc
      .mockResolvedValueOnce({ chats: [summary], nextOffset: null })
      .mockReturnValueOnce(old.promise)
      .mockResolvedValueOnce({ chats: [summary], nextOffset: null })
    const wrapper = await openPanel()
    await selectChat(wrapper)
    await wrapper.get('[data-history-toggle]').trigger('click')
    await wrapper.get('[data-history-toggle]').trigger('click')
    await flushPromises()
    old.resolve(detail)
    await flushPromises()
    expect(wrapper.find('[data-history-detail]').exists()).toBe(false)
    expect(wrapper.text()).not.toContain('Saved answer')
    expect(wrapper.text()).toContain('Select a chat')
  })

  it('handles pending rejections after unmount without issuing additional IPC', async () => {
    const list = deferredReply()
    const chat = deferredReply()
    ipc
      .mockResolvedValueOnce({ chats: [summary], nextOffset: null })
      .mockReturnValueOnce(chat.promise)
      .mockReturnValueOnce(list.promise)
    const wrapper = await openPanel()
    await selectChat(wrapper)
    await wrapper.get('[data-history-refresh]').trigger('click')
    wrapper.unmount()
    panel = undefined
    chat.reject('private stale detail')
    list.reject('private stale list')
    await flushPromises()
    await nextTick()
    expect(ipc).toHaveBeenCalledTimes(3)
  })
})

describe('history IPC contract validation', () => {
  it.each([
    { chats: [{ ...summary, engine: 'unknown' }], nextOffset: null },
    { chats: [{ ...summary, title: null }], nextOffset: null },
    { chats: [{ ...summary, createdAt: 9e15 }], nextOffset: null },
    { chats: [summary], nextOffset: -1 },
    { chats: [summary], nextOffset: 0 }
  ])('rejects an invalid list response %#', async (response) => {
    ipc.mockResolvedValueOnce(response)
    await expect(listAgentChats(0)).rejects.toThrow()
  })

  it.each([
    { ...detail, id: other.id },
    { ...detail, turns: [{ ...detail.turns[0], status: 'running' }] },
    { ...detail, turns: [{ ...detail.turns[0], id: 9007199254740992 }] },
    { ...detail, turns: [{ ...detail.turns[0], id: 'turn-1' }] },
    { ...detail, turns: [{ ...detail.turns[0], assistantText: undefined }] }
  ])('rejects an invalid detail response %#', async (response) => {
    ipc.mockResolvedValueOnce(response)
    await expect(getAgentChat(summary.id)).rejects.toThrow()
  })
})
