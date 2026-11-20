import { invoke } from '@tauri-apps/api/core'
import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createI18n } from 'vue-i18n'
import LegacyHistoryPanel from './LegacyHistoryPanel.vue'
import { legacyHistoryTestMessages } from './messages.fixture'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))

const ipc = vi.mocked(invoke)
const text = legacyHistoryTestMessages['en-US'].tauriPreview.legacyHistory
const MAX_BYTES = 16 * 1024 * 1024
const summary = {
  id: 'legacy-a',
  title: 'Archived conversation A',
  createdAt: 1_700_000_000,
  updatedAt: 1_700_000_060
}
const other = { ...summary, id: 'legacy-b', title: 'Archived conversation B' }
const detail = {
  ...summary,
  messages: [{ role: 'user', text: 'Saved question', status: 'completed' }]
}
const imageAttachment = {
  kind: 'image',
  availability: 'embedded',
  id: 'image-a',
  mediaType: 'image/png'
}
const imported = {
  inserted: 2,
  alreadyImported: 3,
  embeddedAttachments: 4,
  unavailableAttachments: 5
}
let panel: VueWrapper | undefined

beforeEach(() => ipc.mockReset())
afterEach(() => {
  panel?.unmount()
  panel = undefined
})

function mountPanel(locale: 'en-US' | 'zh-CN' = 'en-US'): VueWrapper {
  const i18n = createI18n({ legacy: false, locale, messages: legacyHistoryTestMessages })
  panel = mount(LegacyHistoryPanel, { global: { plugins: [i18n] } })
  return panel
}

async function openPanel(): Promise<VueWrapper> {
  const wrapper = mountPanel()
  await wrapper.get('[data-legacy-toggle]').trigger('click')
  await flushPromises()
  return wrapper
}

async function selectChat(wrapper: VueWrapper, id = summary.id): Promise<void> {
  await wrapper.get(`[data-legacy-chat="${id}"]`).trigger('click')
  await flushPromises()
}

function archiveFile(content: string | Uint8Array): {
  file: File
  read: ReturnType<typeof vi.fn<() => Promise<ArrayBuffer>>>
} {
  const bytes = typeof content === 'string' ? new TextEncoder().encode(content) : content
  const buffer = new Uint8Array(bytes).buffer
  const file = new File([buffer], 'legacy.json', { type: 'application/json' })
  const read = vi.fn(async () => buffer)
  Object.defineProperty(file, 'arrayBuffer', { value: read })
  return { file, read }
}

async function chooseFile(wrapper: VueWrapper, file: File): Promise<void> {
  const picker = wrapper.get<HTMLInputElement>('[data-legacy-file]')
  Object.defineProperty(picker.element, 'files', { value: [file], configurable: true })
  await picker.trigger('change')
}

function deferredReply<T = unknown>(): {
  promise: Promise<T>
  resolve: (value: T) => void
  reject: (reason: unknown) => void
} {
  let resolve!: (value: T) => void
  let reject!: (reason: unknown) => void
  const promise = new Promise<T>((done, fail) => {
    resolve = done
    reject = fail
  })
  return { promise, resolve, reject }
}

describe('explicit archive import', () => {
  it.each(['en-US', 'zh-CN'] as const)(
    'waits for opening and file confirmation (%s)',
    async (locale) => {
      ipc.mockResolvedValueOnce({ chats: [], nextOffset: null })
      const wrapper = mountPanel(locale)
      const labels = legacyHistoryTestMessages[locale].tauriPreview.legacyHistory
      await flushPromises()
      expect(wrapper.get('section').attributes('id')).toBe('legacy-history')
      expect(wrapper.text()).toContain(labels.title)
      expect(ipc).not.toHaveBeenCalled()
      await wrapper.get('[data-legacy-toggle]').trigger('click')
      await flushPromises()
      expect(ipc).toHaveBeenCalledExactlyOnceWith('list_legacy_chats', { limit: 20, offset: 0 })
      expect(wrapper.get('[data-legacy-import]').attributes('disabled')).toBeDefined()
      expect(wrapper.text()).toContain(labels.emptyList)
      expect(wrapper.text()).not.toContain('tauriPreview.legacyHistory.')
    }
  )

  it('imports only after confirmation and displays all counts before refreshing the list', async () => {
    const manifest = '{"archive":"临时夹具"}'
    const { file, read } = archiveFile(manifest)
    ipc
      .mockResolvedValueOnce({ chats: [], nextOffset: null })
      .mockResolvedValueOnce(imported)
      .mockResolvedValueOnce({ chats: [summary], nextOffset: null })
    const wrapper = await openPanel()
    await chooseFile(wrapper, file)
    expect(read).not.toHaveBeenCalled()
    expect(ipc).toHaveBeenCalledTimes(1)
    await wrapper.get('[data-legacy-import]').trigger('click')
    await flushPromises()
    expect(read).toHaveBeenCalledTimes(1)
    expect(ipc.mock.calls).toEqual([
      ['list_legacy_chats', { limit: 20, offset: 0 }],
      ['import_legacy_chat_history', { manifest }],
      ['list_legacy_chats', { limit: 20, offset: 0 }]
    ])
    expect(wrapper.text()).toContain('2 imported')
    expect(wrapper.text()).toContain('3 already imported')
    expect(wrapper.text()).toContain('4 embedded attachments')
    expect(wrapper.text()).toContain('5 unavailable attachments')
    expect(wrapper.text()).toContain(summary.title)
    expect(wrapper.get('[data-legacy-import]').attributes('disabled')).toBeDefined()
    expect(wrapper.text()).not.toContain(manifest)
  })

  it.each(['clear', 'cancel'] as const)(
    'clears a selection without reading or importing (%s)',
    async (action) => {
      ipc.mockResolvedValueOnce({ chats: [], nextOffset: null })
      const { file, read } = archiveFile('{"private":"payload"}')
      const wrapper = await openPanel()
      await chooseFile(wrapper, file)
      if (action === 'clear') await wrapper.get('[data-legacy-clear]').trigger('click')
      else await wrapper.get('[data-legacy-file]').trigger('cancel')
      expect(wrapper.get('[data-legacy-import]').attributes('disabled')).toBeDefined()
      expect(read).not.toHaveBeenCalled()
      expect(ipc).toHaveBeenCalledTimes(1)
      expect(wrapper.find('[role="alert"]').exists()).toBe(false)
    }
  )

  it('rejects an oversized file before reading it', async () => {
    ipc.mockResolvedValueOnce({ chats: [], nextOffset: null })
    const { file, read } = archiveFile('{}')
    Object.defineProperty(file, 'size', { value: MAX_BYTES + 1 })
    const wrapper = await openPanel()
    await chooseFile(wrapper, file)
    await wrapper.get('[data-legacy-import]').trigger('click')
    await flushPromises()
    expect(read).not.toHaveBeenCalled()
    expect(ipc).toHaveBeenCalledTimes(1)
    expect(wrapper.text()).toContain(text.errors.tooLarge)
  })

  it('accepts exactly 16 MiB of UTF-8 bytes', async () => {
    const json = '{"value":"中"}'
    const jsonBytes = new TextEncoder().encode(json).byteLength
    const manifest = `${' '.repeat(MAX_BYTES - jsonBytes)}${json}`
    const { file } = archiveFile(manifest)
    expect(file.size).toBe(MAX_BYTES)
    ipc
      .mockResolvedValueOnce({ chats: [], nextOffset: null })
      .mockResolvedValueOnce(imported)
      .mockResolvedValueOnce({ chats: [], nextOffset: null })
    const wrapper = await openPanel()
    await chooseFile(wrapper, file)
    await wrapper.get('[data-legacy-import]').trigger('click')
    await flushPromises()
    expect(ipc.mock.calls[1]?.[0]).toBe('import_legacy_chat_history')
    expect(ipc.mock.calls[1]?.[1]).toEqual({ manifest })
    expect(wrapper.find('[role="alert"]').exists()).toBe(false)
  })

  it('uses fatal UTF-8 decoding and never sends malformed bytes to the host', async () => {
    ipc.mockResolvedValueOnce({ chats: [], nextOffset: null })
    const { file } = archiveFile(new Uint8Array([0xc3, 0x28]))
    const wrapper = await openPanel()
    await chooseFile(wrapper, file)
    await wrapper.get('[data-legacy-import]').trigger('click')
    await flushPromises()
    expect(ipc).toHaveBeenCalledTimes(1)
    expect(wrapper.text()).toContain(text.errors.invalidUtf8)
  })

  it('hides file-read and import rejection payloads and permits retry', async () => {
    ipc
      .mockResolvedValueOnce({ chats: [], nextOffset: null })
      .mockRejectedValueOnce('private manifest /tmp/archive.json token=fixture')
      .mockResolvedValueOnce(imported)
      .mockResolvedValueOnce({ chats: [], nextOffset: null })
    const { file, read } = archiveFile('{"private":"manifest"}')
    read.mockRejectedValueOnce(new Error('private file /tmp/archive.json'))
    const wrapper = await openPanel()
    await chooseFile(wrapper, file)
    await wrapper.get('[data-legacy-import]').trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain(text.errors.readFailed)
    expect(ipc).toHaveBeenCalledTimes(1)
    await wrapper.get('[data-legacy-import]').trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain(text.errors.importFailed)
    expect(wrapper.text()).not.toContain('/tmp/archive.json')
    expect(wrapper.text()).not.toContain('private')
    expect(wrapper.text()).not.toContain('token=fixture')
    await wrapper.get('[data-legacy-import]').trigger('click')
    await flushPromises()
    expect(wrapper.find('[role="alert"]').exists()).toBe(false)
  })

  it('releases the import guard after the receipt while a list refresh is still pending', async () => {
    const list = deferredReply()
    ipc
      .mockResolvedValueOnce({ chats: [], nextOffset: null })
      .mockResolvedValueOnce(imported)
      .mockReturnValueOnce(list.promise)
    const wrapper = await openPanel()
    await chooseFile(wrapper, archiveFile('{}').file)
    await wrapper.get('[data-legacy-import]').trigger('click')
    await flushPromises()
    expect(wrapper.find('[data-legacy-imported]').exists()).toBe(true)
    expect(wrapper.text()).toContain(text.loadingList)
    expect(wrapper.get('[data-legacy-file]').attributes('disabled')).toBeUndefined()
    await chooseFile(wrapper, archiveFile('{}').file)
    expect(wrapper.get('[data-legacy-import]').attributes('disabled')).toBeUndefined()
    list.resolve({ chats: [], nextOffset: null })
    await flushPromises()
  })

  it('blocks duplicate confirmations and abandons the read if the panel closes', async () => {
    const slow = deferredReply<ArrayBuffer>()
    const { file, read } = archiveFile('{}')
    read.mockReturnValueOnce(slow.promise)
    ipc.mockResolvedValueOnce({ chats: [], nextOffset: null })
    const wrapper = await openPanel()
    await chooseFile(wrapper, file)
    await wrapper.get('[data-legacy-import]').trigger('click')
    await wrapper.get('[data-legacy-import]').trigger('click')
    expect(read).toHaveBeenCalledTimes(1)
    await wrapper.get('[data-legacy-toggle]').trigger('click')
    slow.resolve(new TextEncoder().encode('{}').buffer)
    await flushPromises()
    expect(ipc).toHaveBeenCalledTimes(1)
    expect(wrapper.find('[data-legacy-content]').exists()).toBe(false)
  })
})

describe('archive browsing', () => {
  it('paginates using the host offset, de-duplicates overlapping pages and refreshes from zero', async () => {
    const more = deferredReply()
    ipc
      .mockResolvedValueOnce({ chats: [summary], nextOffset: 31 })
      .mockReturnValueOnce(more.promise)
      .mockResolvedValueOnce({ chats: [other], nextOffset: null })
    const wrapper = await openPanel()
    await wrapper.get('[data-legacy-more]').trigger('click')
    await wrapper.get('[data-legacy-more]').trigger('click')
    expect(ipc).toHaveBeenCalledTimes(2)
    more.resolve({ chats: [summary, other], nextOffset: null })
    await flushPromises()
    expect(wrapper.findAll('[data-legacy-chat]')).toHaveLength(2)
    await wrapper.get('[data-legacy-refresh]').trigger('click')
    await flushPromises()
    expect(ipc.mock.calls).toEqual([
      ['list_legacy_chats', { limit: 20, offset: 0 }],
      ['list_legacy_chats', { limit: 20, offset: 31 }],
      ['list_legacy_chats', { limit: 20, offset: 0 }]
    ])
    expect(wrapper.findAll('[data-legacy-chat]')).toHaveLength(1)
  })

  it('retries a first-read failure, preserves rows on later failures, and hides raw errors', async () => {
    ipc
      .mockRejectedValueOnce('private /tmp/archive.sqlite')
      .mockResolvedValueOnce({ chats: [summary], nextOffset: 20 })
      .mockRejectedValueOnce('private page')
      .mockRejectedValueOnce('private refresh')
    const wrapper = await openPanel()
    expect(wrapper.text()).toContain(text.listError)
    expect(wrapper.text()).not.toContain(text.emptyList)
    await wrapper.get('[data-legacy-refresh]').trigger('click')
    await flushPromises()
    await wrapper.get('[data-legacy-more]').trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain(summary.title)
    expect(wrapper.find('[data-legacy-more]').exists()).toBe(true)
    await wrapper.get('[data-legacy-refresh]').trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain(summary.title)
    expect(wrapper.text()).not.toContain('private')
    expect(wrapper.text()).not.toContain('/tmp/archive.sqlite')
  })

  it.each(['en-US', 'zh-CN'] as const)(
    'shows messages, statuses and Unix seconds as plain text (%s)',
    async (locale) => {
      const title = '<img src=x onerror="bad()"> **title**'
      const message = '<script>bad()</script>\n[run](javascript:bad())'
      const statuses = ['completed', 'failed', 'cancelled', 'interrupted'] as const
      ipc
        .mockResolvedValueOnce({ chats: [{ ...summary, title }], nextOffset: null })
        .mockResolvedValueOnce({
          ...summary,
          title,
          messages: statuses.map((status, index) => ({
            role: index % 2 === 0 ? 'user' : 'assistant',
            text: message,
            status
          }))
        })
      const wrapper = mountPanel(locale)
      await wrapper.get('[data-legacy-toggle]').trigger('click')
      await flushPromises()
      expect(ipc).toHaveBeenCalledTimes(1)
      await selectChat(wrapper)
      const labels = legacyHistoryTestMessages[locale].tauriPreview.legacyHistory
      expect(wrapper.get('[data-legacy-detail] h3').text()).toBe(title)
      expect(wrapper.get('[data-legacy-message-text]').element.textContent).toBe(message)
      expect(wrapper.text()).toContain(labels.roles.user)
      expect(wrapper.text()).toContain(labels.roles.assistant)
      for (const status of statuses) expect(wrapper.text()).toContain(labels.statuses[status])
      expect(wrapper.get('[data-legacy-created] time').attributes('datetime')).toBe(
        '2023-11-14T22:13:20.000Z'
      )
      expect(wrapper.get('[data-legacy-created] time').text()).toBe(
        new Date(summary.createdAt * 1000).toLocaleString(locale)
      )
      expect(wrapper.find('img, script, a, b, textarea, form').exists()).toBe(false)
      expect(wrapper.text()).not.toContain('tauriPreview.legacyHistory.')
    }
  )

  it('retries a rejected detail and shows an empty conversation', async () => {
    ipc
      .mockResolvedValueOnce({ chats: [summary], nextOffset: null })
      .mockRejectedValueOnce('private message contents')
      .mockResolvedValueOnce({ ...summary, title: '', messages: [] })
    const wrapper = await openPanel()
    await selectChat(wrapper)
    expect(wrapper.text()).toContain(text.detailError)
    expect(wrapper.text()).not.toContain('private message contents')
    await wrapper.get('[data-legacy-retry]').trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain(text.untitled)
    expect(wrapper.text()).toContain(text.emptyMessages)
  })
})

describe('archived attachment previews', () => {
  it.each(['image/png', 'image/jpeg', 'image/webp', 'image/gif'])(
    'reads %s only after clicking and uses its fixed data prefix',
    async (mediaType) => {
      const image = deferredReply()
      ipc
        .mockResolvedValueOnce({ chats: [summary], nextOffset: null })
        .mockResolvedValueOnce({
          ...summary,
          messages: [{ ...detail.messages[0], attachments: [{ ...imageAttachment, mediaType }] }]
        })
        .mockReturnValueOnce(image.promise)
      const wrapper = await openPanel()
      await selectChat(wrapper)
      expect(ipc).toHaveBeenCalledTimes(2)
      expect(wrapper.find('img').exists()).toBe(false)
      await wrapper.get('[data-legacy-image]').trigger('click')
      await wrapper.get('[data-legacy-image]').trigger('click')
      expect(ipc).toHaveBeenCalledTimes(3)
      expect(ipc.mock.calls[2]).toEqual([
        'read_legacy_chat_attachment',
        { attachmentId: imageAttachment.id }
      ])
      image.resolve({ mediaType, base64: 'AQID' })
      await flushPromises()
      expect(wrapper.get('img').attributes('src')).toBe(`data:${mediaType};base64,AQID`)
      expect(wrapper.get('img').attributes('alt')).toBe(text.attachments.imageAlt)
      expect(wrapper.find('a, iframe, object, embed').exists()).toBe(false)
    }
  )

  it('marks unavailable, unsupported and incomplete attachments unavailable without reading them', async () => {
    const attachments = [
      { ...imageAttachment, availability: 'unavailable' },
      { ...imageAttachment, mediaType: 'image/svg+xml' },
      { ...imageAttachment, mediaType: 'text/html' },
      { ...imageAttachment, kind: 'file' },
      { kind: 'image', availability: 'embedded', mediaType: 'image/png' },
      { kind: 'image', availability: 'embedded', id: 'missing-media-type' },
      { kind: '<script>bad()</script>', availability: 'unavailable' }
    ]
    ipc
      .mockResolvedValueOnce({ chats: [summary], nextOffset: null })
      .mockResolvedValueOnce({ ...summary, messages: [{ ...detail.messages[0], attachments }] })
    const wrapper = await openPanel()
    await selectChat(wrapper)
    expect(wrapper.findAll('[data-legacy-unavailable]')).toHaveLength(attachments.length)
    expect(wrapper.find('[data-legacy-image]').exists()).toBe(false)
    expect(wrapper.find('img, script, a, iframe').exists()).toBe(false)
    expect(ipc).toHaveBeenCalledTimes(2)
  })

  it.each([
    { mediaType: 'image/svg+xml', base64: 'AQID' },
    { mediaType: 'text/html', base64: 'AQID' },
    { mediaType: 'image/jpeg', base64: 'AQID' },
    { mediaType: 'image/png', base64: 'AQID" onerror="bad()' },
    { mediaType: 'image/png', base64: 'data:text/html;base64,AQID' }
  ])('rejects unsafe or mismatched image responses %# and permits retry', async (response) => {
    ipc
      .mockResolvedValueOnce({ chats: [summary], nextOffset: null })
      .mockResolvedValueOnce({
        ...summary,
        messages: [{ ...detail.messages[0], attachments: [imageAttachment] }]
      })
      .mockResolvedValueOnce(response)
      .mockResolvedValueOnce({ mediaType: 'image/png', base64: 'AQID' })
    const wrapper = await openPanel()
    await selectChat(wrapper)
    await wrapper.get('[data-legacy-image]').trigger('click')
    await flushPromises()
    expect(wrapper.find('img').exists()).toBe(false)
    expect(wrapper.text()).toContain(text.attachments.imageError)
    expect(wrapper.text()).not.toContain(response.base64)
    await wrapper.get('[data-legacy-image]').trigger('click')
    await flushPromises()
    expect(wrapper.get('img').attributes('src')).toBe('data:image/png;base64,AQID')
    expect(wrapper.text()).not.toContain(text.attachments.imageError)
  })

  it('handles a browser image decode error with a safe retry state', async () => {
    ipc
      .mockResolvedValueOnce({ chats: [summary], nextOffset: null })
      .mockResolvedValueOnce({
        ...summary,
        messages: [{ ...detail.messages[0], attachments: [imageAttachment] }]
      })
      .mockResolvedValueOnce({ mediaType: 'image/png', base64: 'AQID' })
    const wrapper = await openPanel()
    await selectChat(wrapper)
    await wrapper.get('[data-legacy-image]').trigger('click')
    await flushPromises()
    await wrapper.get('img').trigger('error')
    expect(wrapper.find('img').exists()).toBe(false)
    expect(wrapper.text()).toContain(text.attachments.imageError)
    expect(wrapper.get('[data-legacy-image]').text()).toBe(text.attachments.retryImage)
  })
})

describe('archive asynchronous ordering', () => {
  it.each(['resolve', 'reject'] as const)(
    'discards an older list %s after refresh',
    async (settle) => {
      const old = deferredReply()
      ipc
        .mockReturnValueOnce(old.promise)
        .mockResolvedValueOnce({ chats: [other], nextOffset: null })
      const wrapper = await openPanel()
      await wrapper.get('[data-legacy-refresh]').trigger('click')
      await flushPromises()
      if (settle === 'resolve') old.resolve({ chats: [summary], nextOffset: 20 })
      else old.reject('private obsolete archive')
      await flushPromises()
      expect(wrapper.text()).toContain(other.title)
      expect(wrapper.text()).not.toContain(summary.title)
      expect(wrapper.find('[role="alert"]').exists()).toBe(false)
      expect(wrapper.find('[data-legacy-more]').exists()).toBe(false)
    }
  )

  it.each(['resolve', 'reject'] as const)(
    'discards an older detail %s after switching chats',
    async (settle) => {
      const old = deferredReply()
      ipc
        .mockResolvedValueOnce({ chats: [summary, other], nextOffset: null })
        .mockReturnValueOnce(old.promise)
        .mockResolvedValueOnce({ ...other, messages: [] })
      const wrapper = await openPanel()
      await selectChat(wrapper)
      await selectChat(wrapper, other.id)
      if (settle === 'resolve') old.resolve(detail)
      else old.reject('private obsolete message')
      await flushPromises()
      expect(wrapper.get('[data-legacy-detail] h3').text()).toBe(other.title)
      expect(wrapper.find('[role="alert"]').exists()).toBe(false)
      expect(wrapper.text()).not.toContain('Saved question')
    }
  )

  it.each(['resolve', 'reject'] as const)(
    'discards an older image %s after switching chats',
    async (settle) => {
      const old = deferredReply()
      ipc
        .mockResolvedValueOnce({ chats: [summary, other], nextOffset: null })
        .mockResolvedValueOnce({
          ...summary,
          messages: [{ ...detail.messages[0], attachments: [imageAttachment] }]
        })
        .mockReturnValueOnce(old.promise)
        .mockResolvedValueOnce({ ...other, messages: [] })
      const wrapper = await openPanel()
      await selectChat(wrapper)
      await wrapper.get('[data-legacy-image]').trigger('click')
      await selectChat(wrapper, other.id)
      if (settle === 'resolve') old.resolve({ mediaType: 'image/png', base64: 'AQID' })
      else old.reject('private obsolete image')
      await flushPromises()
      expect(wrapper.get('[data-legacy-detail] h3').text()).toBe(other.title)
      expect(wrapper.find('img').exists()).toBe(false)
      expect(wrapper.find('[role="alert"]').exists()).toBe(false)
    }
  )

  it('discards a pending page and detail when refresh replaces the list', async () => {
    const oldPage = deferredReply()
    const oldChat = deferredReply()
    ipc
      .mockResolvedValueOnce({ chats: [summary], nextOffset: 20 })
      .mockReturnValueOnce(oldChat.promise)
      .mockReturnValueOnce(oldPage.promise)
      .mockResolvedValueOnce({ chats: [other], nextOffset: null })
    const wrapper = await openPanel()
    await selectChat(wrapper)
    await wrapper.get('[data-legacy-more]').trigger('click')
    await wrapper.get('[data-legacy-refresh]').trigger('click')
    await flushPromises()
    oldPage.resolve({ chats: [summary], nextOffset: 40 })
    oldChat.resolve(detail)
    await flushPromises()
    expect(wrapper.findAll('[data-legacy-chat]')).toHaveLength(1)
    expect(wrapper.text()).toContain(other.title)
    expect(wrapper.find('[data-legacy-detail]').exists()).toBe(false)
  })

  it('discards old list responses across closing and reopening', async () => {
    const old = deferredReply()
    ipc.mockReturnValueOnce(old.promise).mockResolvedValueOnce({ chats: [other], nextOffset: null })
    const wrapper = await openPanel()
    await wrapper.get('[data-legacy-toggle]').trigger('click')
    await wrapper.get('[data-legacy-toggle]').trigger('click')
    await flushPromises()
    old.resolve({ chats: [summary], nextOffset: 20 })
    await flushPromises()
    expect(wrapper.text()).toContain(other.title)
    expect(wrapper.text()).not.toContain(summary.title)
  })

  it('does not publish an import result after closing and retains the in-flight import guard', async () => {
    const pending = deferredReply()
    ipc
      .mockResolvedValueOnce({ chats: [], nextOffset: null })
      .mockReturnValueOnce(pending.promise)
      .mockResolvedValueOnce({ chats: [], nextOffset: null })
    const wrapper = await openPanel()
    await chooseFile(wrapper, archiveFile('{}').file)
    await wrapper.get('[data-legacy-import]').trigger('click')
    await flushPromises()
    await wrapper.get('[data-legacy-toggle]').trigger('click')
    await wrapper.get('[data-legacy-toggle]').trigger('click')
    await flushPromises()
    expect(wrapper.get('[data-legacy-file]').attributes('disabled')).toBeDefined()
    expect(wrapper.get('[data-legacy-import]').attributes('disabled')).toBeDefined()
    pending.resolve(imported)
    await flushPromises()
    expect(wrapper.find('[data-legacy-imported]').exists()).toBe(false)
    expect(wrapper.get('[data-legacy-file]').attributes('disabled')).toBeUndefined()
    expect(ipc).toHaveBeenCalledTimes(3)
  })

  it('handles pending image and list rejections after unmount without further IPC', async () => {
    const oldImage = deferredReply()
    const oldList = deferredReply()
    ipc
      .mockResolvedValueOnce({ chats: [summary], nextOffset: null })
      .mockResolvedValueOnce({
        ...summary,
        messages: [{ ...detail.messages[0], attachments: [imageAttachment] }]
      })
      .mockReturnValueOnce(oldImage.promise)
      .mockReturnValueOnce(oldList.promise)
    const wrapper = await openPanel()
    await selectChat(wrapper)
    await wrapper.get('[data-legacy-image]').trigger('click')
    await wrapper.get('[data-legacy-refresh]').trigger('click')
    wrapper.unmount()
    panel = undefined
    oldImage.reject('private image')
    oldList.reject('private list')
    await flushPromises()
    expect(ipc).toHaveBeenCalledTimes(4)
  })
})
