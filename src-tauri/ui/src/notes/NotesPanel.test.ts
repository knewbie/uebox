import { invoke } from '@tauri-apps/api/core'
import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import NotesPanel from './NotesPanel.vue'
import { createAppI18n } from '../i18n'
import en from '@renderer/i18n/locales/en-US'
import zh from '@renderer/i18n/locales/zh-CN'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))

const ipc = vi.mocked(invoke)
const book = { id: '9007199254740993', title: 'Fixture notebook' }
const anotherBook = { id: '9007199254740994', title: 'Another notebook' }
const standalone = { id: '9007199254740995', title: 'Standalone fixture', notebookId: null }
const anotherNote = { ...standalone, id: '9007199254740996', title: 'Another note' }
const child = { id: '9007199254740997', title: 'Notebook note', notebookId: book.id }
const raw =
  '\n# Markdown source\n<script>bad()</script>\n[run](javascript:bad())\n  trailing spaces  \n'
const emptyPage = { items: [], nextId: null }
let panel: VueWrapper | undefined

beforeEach(() => ipc.mockReset())
afterEach(() => {
  panel?.unmount()
  panel = undefined
})

function mountPanel(locale: 'en-US' | 'zh-CN' = 'en-US'): VueWrapper {
  const i18n = createAppI18n(locale)
  panel = mount(NotesPanel, { global: { plugins: [i18n] } })
  return panel
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

async function click(wrapper: VueWrapper, selector: string): Promise<void> {
  await wrapper.get(selector).trigger('click')
  await flushPromises()
}

it('uses separate bounded cursors and ignores an old notebook scope response after returning to standalone notes', async () => {
  const old = deferredReply()
  ipc
    .mockResolvedValueOnce({ items: [book], nextId: book.id })
    .mockResolvedValueOnce({ items: [standalone], nextId: standalone.id })
    .mockResolvedValueOnce({ items: [anotherBook], nextId: '' })
    .mockResolvedValueOnce({ items: [anotherNote], nextId: null })
    .mockReturnValueOnce(old.promise)
    .mockResolvedValueOnce({ items: [standalone], nextId: null })
  const wrapper = mountPanel('zh-CN')
  await flushPromises()
  expect(ipc.mock.calls).toEqual([
    ['list_notebooks', { afterId: null, limit: 20 }],
    ['list_notes', { notebookId: null, afterId: null, limit: 20 }]
  ])
  expect(wrapper.find('[data-note-content]').exists()).toBe(false)
  await click(wrapper, '[data-notebook-more]')
  await click(wrapper, '[data-notes-more]')
  expect(ipc.mock.calls[2]).toEqual(['list_notebooks', { afterId: book.id, limit: 20 }])
  expect(ipc.mock.calls[3]).toEqual([
    'list_notes',
    { notebookId: null, afterId: standalone.id, limit: 20 }
  ])
  expect(wrapper.findAll('[data-notebook-select]')).toHaveLength(2)
  expect(wrapper.findAll('[data-note-select]')).toHaveLength(2)
  expect(wrapper.find('[data-notebook-more]').exists()).toBe(false)
  expect(wrapper.find('[data-notes-more]').exists()).toBe(false)
  await click(wrapper, `[data-notebook-select="${book.id}"]`)
  await click(wrapper, '[data-notes-standalone]')
  old.resolve({ items: [child], nextId: child.id })
  await flushPromises()
  expect(wrapper.find(`[data-note-select="${standalone.id}"]`).exists()).toBe(true)
  expect(wrapper.find(`[data-note-select="${child.id}"]`).exists()).toBe(false)
  expect(wrapper.get('[data-notes-standalone]').attributes('aria-pressed')).toBe('true')
  expect(ipc.mock.calls.map(([command]) => command)).not.toContain('get_note')
  expect(wrapper.text()).not.toContain('tauriPreview.notes.')
})

it('creates and renames a notebook and explicitly saves raw source in notebook and standalone notes', async () => {
  const renamed = { ...book, title: 'Host notebook title' }
  const savedChild = { ...child, title: 'Saved child title', content: raw }
  ipc
    .mockResolvedValueOnce(emptyPage)
    .mockResolvedValueOnce(emptyPage)
    .mockResolvedValueOnce(book)
    .mockResolvedValueOnce(emptyPage)
    .mockResolvedValueOnce(renamed)
    .mockResolvedValueOnce(savedChild)
    .mockResolvedValueOnce(emptyPage)
    .mockResolvedValueOnce({ ...standalone, content: raw })
  const wrapper = mountPanel()
  await flushPromises()
  await wrapper.get('[data-notebook-create-form] input').setValue(' New notebook ')
  await wrapper.get('[data-notebook-create-form]').trigger('submit')
  await flushPromises()
  expect(ipc.mock.calls[2]).toEqual(['save_notebook', { id: null, title: 'New notebook' }])
  expect(wrapper.get('[data-notes-standalone]').attributes('aria-pressed')).toBe('true')
  await click(wrapper, `[data-notebook-select="${book.id}"]`)
  await wrapper.get('[data-notebook-save-form] input').setValue(' Renamed notebook ')
  await wrapper.get('[data-notebook-save-form]').trigger('submit')
  await flushPromises()
  expect(ipc.mock.calls[4]).toEqual(['save_notebook', { id: book.id, title: 'Renamed notebook' }])
  expect(wrapper.text()).toContain(renamed.title)
  await click(wrapper, '[data-note-new]')
  await wrapper.get('[data-note-editor] input').setValue(' Child title ')
  await wrapper.get('[data-note-editor] textarea').setValue(raw)
  expect(ipc).toHaveBeenCalledTimes(5)
  await wrapper.get('[data-note-editor]').trigger('submit')
  await flushPromises()
  expect(ipc.mock.calls[5]).toEqual([
    'save_note',
    { id: null, notebookId: book.id, title: 'Child title', content: raw }
  ])
  expect(wrapper.get('[data-note-content]').element.textContent).toBe(raw)
  expect(wrapper.text()).toContain(savedChild.title)
  expect(wrapper.find('script, a, img').exists()).toBe(false)
  await click(wrapper, '[data-notes-standalone]')
  await click(wrapper, '[data-note-new]')
  await wrapper.get('[data-note-editor] input').setValue(standalone.title)
  await wrapper.get('[data-note-editor] textarea').setValue(raw)
  await wrapper.get('[data-note-editor]').trigger('submit')
  await flushPromises()
  expect(ipc.mock.calls[7]).toEqual([
    'save_note',
    { id: null, notebookId: null, title: standalone.title, content: raw }
  ])
  expect(wrapper.get('[data-note-content]').element.textContent).toBe(raw)
})

it('keeps dirty source and hides save errors, then requires approval before abandoning the draft', async () => {
  const save = deferredReply()
  const text = en.tauriPreview.notes
  ipc
    .mockResolvedValueOnce({ items: [book], nextId: null })
    .mockResolvedValueOnce({ items: [standalone, anotherNote], nextId: null })
    .mockResolvedValueOnce({ ...standalone, content: raw })
    .mockReturnValueOnce(save.promise)
    .mockResolvedValueOnce({ ...anotherNote, content: 'Another saved body' })
  const wrapper = mountPanel()
  await flushPromises()
  await click(wrapper, `[data-note-select="${standalone.id}"]`)
  expect(ipc.mock.calls[2]).toEqual(['get_note', { noteId: standalone.id }])
  expect(wrapper.get('[data-note-content]').element.textContent).toBe(raw)
  await click(wrapper, '[data-note-edit]')
  const draft = `${raw}unsaved changes\n`
  await wrapper.get('[data-note-editor] textarea').setValue(draft)
  await click(wrapper, `[data-note-select="${anotherNote.id}"]`)
  expect(wrapper.get('[data-notes-discard-confirm]').text()).toContain(text.unsaved)
  expect(ipc).toHaveBeenCalledTimes(3)
  await click(wrapper, '[data-notes-discard-cancel]')
  expect(wrapper.get<HTMLTextAreaElement>('[data-note-editor] textarea').element.value).toBe(draft)
  await wrapper.get('[data-note-editor]').trigger('submit')
  await wrapper.get('[data-note-editor]').trigger('submit')
  expect(ipc).toHaveBeenCalledTimes(4)
  expect(ipc.mock.calls[3]).toEqual([
    'save_note',
    { id: standalone.id, notebookId: null, title: standalone.title, content: draft }
  ])
  save.reject(new Error('private /fixture/notes.sqlite encoded-payload'))
  await flushPromises()
  expect(wrapper.get('[data-notes-write-error]').text()).toBe(text.saveError)
  expect(wrapper.get<HTMLTextAreaElement>('[data-note-editor] textarea').element.value).toBe(draft)
  expect(wrapper.text()).not.toContain('/fixture/notes.sqlite')
  await click(wrapper, `[data-note-select="${anotherNote.id}"]`)
  expect(ipc).toHaveBeenCalledTimes(4)
  await click(wrapper, '[data-notes-discard-approve]')
  expect(ipc.mock.calls[4]).toEqual(['get_note', { noteId: anotherNote.id }])
  expect(wrapper.get('[data-note-content]').text()).toBe('Another saved body')
})

it('confirms notebook cascading deletion and preserves standalone notes without deleting child notes individually', async () => {
  const removing = deferredReply()
  const text = zh.tauriPreview.notes
  ipc
    .mockResolvedValueOnce({ items: [book], nextId: null })
    .mockResolvedValueOnce({ items: [standalone], nextId: null })
    .mockResolvedValueOnce({ items: [child], nextId: null })
    .mockResolvedValueOnce({ ...child, content: raw })
    .mockReturnValueOnce(removing.promise)
    .mockResolvedValueOnce({ items: [standalone], nextId: null })
  const wrapper = mountPanel('zh-CN')
  await flushPromises()
  await click(wrapper, `[data-notebook-select="${book.id}"]`)
  await click(wrapper, `[data-note-select="${child.id}"]`)
  await click(wrapper, '[data-notebook-remove]')
  expect(wrapper.get('[data-notes-delete-confirm]').text()).toContain(
    text.confirmDeleteNotebook.replace('{title}', book.title)
  )
  expect(ipc).toHaveBeenCalledTimes(4)
  await click(wrapper, '[data-notes-delete-cancel]')
  expect(ipc).toHaveBeenCalledTimes(4)
  await click(wrapper, '[data-notebook-remove]')
  await click(wrapper, '[data-notes-delete-approve]')
  await click(wrapper, '[data-notes-delete-approve]')
  expect(ipc).toHaveBeenCalledTimes(5)
  removing.resolve(true)
  await flushPromises()
  expect(ipc.mock.calls[4]).toEqual(['remove_notebook', { notebookId: book.id }])
  expect(ipc.mock.calls[5]).toEqual(['list_notes', { notebookId: null, afterId: null, limit: 20 }])
  expect(ipc.mock.calls.map(([command]) => command)).not.toContain('remove_note')
  expect(wrapper.find(`[data-notebook-select="${book.id}"]`).exists()).toBe(false)
  expect(wrapper.find(`[data-note-select="${standalone.id}"]`).exists()).toBe(true)
  expect(wrapper.find('[data-note-content]').exists()).toBe(false)
  expect(wrapper.text()).toContain(text.emptyNotebooks)
})

it('deletes a note only after approval and preserves it when the host returns false', async () => {
  const text = en.tauriPreview.notes
  ipc
    .mockResolvedValueOnce(emptyPage)
    .mockResolvedValueOnce({ items: [standalone], nextId: null })
    .mockResolvedValueOnce({ ...standalone, content: raw })
    .mockResolvedValueOnce(false)
    .mockResolvedValueOnce(true)
  const wrapper = mountPanel()
  await flushPromises()
  await click(wrapper, `[data-note-select="${standalone.id}"]`)
  await click(wrapper, '[data-note-remove]')
  expect(wrapper.get('[data-notes-delete-confirm]').text()).toContain(
    text.confirmDeleteNote.replace('{title}', standalone.title)
  )
  await click(wrapper, '[data-notes-delete-cancel]')
  expect(ipc).toHaveBeenCalledTimes(3)
  await click(wrapper, '[data-note-remove]')
  await click(wrapper, '[data-notes-delete-approve]')
  expect(wrapper.get('[data-notes-write-error]').text()).toBe(text.deleteError)
  expect(wrapper.find(`[data-note-select="${standalone.id}"]`).exists()).toBe(true)
  await click(wrapper, '[data-notes-delete-approve]')
  expect(wrapper.find(`[data-note-select="${standalone.id}"]`).exists()).toBe(false)
  expect(wrapper.find('[data-note-content]').exists()).toBe(false)
  expect(wrapper.text()).toContain(text.emptyNotes)
  expect(ipc.mock.calls.slice(3)).toEqual([
    ['remove_note', { noteId: standalone.id }],
    ['remove_note', { noteId: standalone.id }]
  ])
})

it('imports a confirmed archive and refreshes lists while leaving the current draft intact', async () => {
  const manifest = '{"format":"uebox-notes","version":1}'
  const read = vi.fn(async () => new TextEncoder().encode(manifest).buffer)
  const file = new File([manifest], 'notes.json', { type: 'application/json' })
  Object.defineProperty(file, 'arrayBuffer', { value: read })
  ipc
    .mockResolvedValueOnce(emptyPage)
    .mockResolvedValueOnce(emptyPage)
    .mockResolvedValueOnce({
      notebooksInserted: 1,
      notebooksExisting: 0,
      notesInserted: 2,
      notesExisting: 0
    })
    .mockResolvedValueOnce({ items: [book], nextId: null })
    .mockResolvedValueOnce({ items: [standalone], nextId: null })
  const wrapper = mountPanel()
  await flushPromises()
  await click(wrapper, '[data-note-new]')
  await wrapper.get('[data-note-editor] textarea').setValue(raw)
  const picker = wrapper.get('#legacy-notes-file')
  Object.defineProperty(picker.element, 'files', { value: [file] })
  await picker.trigger('change')
  expect(read).not.toHaveBeenCalled()
  expect(ipc).toHaveBeenCalledTimes(2)

  await click(wrapper, '[data-notes-import-confirm]')
  expect(ipc.mock.calls.slice(2)).toEqual([
    ['import_legacy_notes', { manifest }],
    ['list_notebooks', { afterId: null, limit: 20 }],
    ['list_notes', { notebookId: null, afterId: null, limit: 20 }]
  ])
  expect(wrapper.text()).toContain('Added 1 notebooks and 2 notes.')
  expect(wrapper.get<HTMLTextAreaElement>('[data-note-editor] textarea').element.value).toBe(raw)
  expect(wrapper.find('[data-notes-discard-confirm]').exists()).toBe(false)
})

it('rejects an oversized legacy file before reading or invoking the host', async () => {
  ipc.mockResolvedValue(emptyPage)
  const wrapper = mountPanel()
  await flushPromises()
  const file = new File([], 'oversized.json')
  const read = vi.fn()
  Object.defineProperty(file, 'size', { value: 16 * 1024 * 1024 + 1 })
  Object.defineProperty(file, 'arrayBuffer', { value: read })
  const picker = wrapper.get('#legacy-notes-file')
  Object.defineProperty(picker.element, 'files', { value: [file] })
  await picker.trigger('change')

  await click(wrapper, '[data-notes-import-confirm]')
  expect(wrapper.text()).toContain(en.tauriPreview.notes.importLimit)
  expect(read).not.toHaveBeenCalled()
  expect(ipc).toHaveBeenCalledTimes(2)
})
