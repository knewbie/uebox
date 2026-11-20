import { invoke } from '@tauri-apps/api/core'
import { flushPromises, mount, type DOMWrapper, type VueWrapper } from '@vue/test-utils'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { createI18n } from 'vue-i18n'
import type { RegisteredProject } from '../commands'
import ProjectCollectionsPanel from './ProjectCollectionsPanel.vue'
import { projectCollectionsTestMessages } from './messages.fixture'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))

const ipc = vi.mocked(invoke)
const projects: readonly RegisteredProject[] = Object.freeze([
  Object.freeze({
    id: '9007199254740995',
    engine: 'unreal' as const,
    name: '<b>Offline UE project</b>',
    path: '/fixture/offline/UE'
  }),
  Object.freeze({
    id: '9007199254740994',
    engine: 'unity' as const,
    name: 'Unity fixture',
    path: '/fixture/offline/Unity'
  })
])
const first = { id: '9007199254740993', name: null as string | null, projectIds: [projects[0].id] }
const second = { id: '12', name: '<img src=x onerror="bad()">', projectIds: [] as string[] }
let panel: VueWrapper | undefined

beforeEach(() => ipc.mockReset())
afterEach(() => {
  panel?.unmount()
  panel = undefined
})

function mountPanel(locale: 'en-US' | 'zh-CN' = 'en-US'): VueWrapper {
  const i18n = createI18n({ legacy: false, locale, messages: projectCollectionsTestMessages })
  panel = mount(ProjectCollectionsPanel, { props: { projects }, global: { plugins: [i18n] } })
  return panel
}

function order(wrapper: VueWrapper): Array<string | undefined> {
  return wrapper
    .findAll('[data-collection-row]')
    .map((row) => row.attributes('data-collection-row'))
}

function checkbox(
  wrapper: VueWrapper,
  registrationId: string
): Omit<DOMWrapper<HTMLButtonElement>, 'exists'> {
  return wrapper.get<HTMLButtonElement>(
    `[data-collection-project="${registrationId}"] [role="checkbox"]`
  )
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

it('loads in host order, renders nullable names and project props as text, and blocks writes while loading', async () => {
  const list = deferredReply()
  ipc.mockReturnValueOnce(list.promise)
  const wrapper = mountPanel('zh-CN')
  const text = projectCollectionsTestMessages['zh-CN'].tauriPreview.projectCollections
  await flushPromises()
  expect(wrapper.text()).toContain(text.loading)
  expect(wrapper.text()).not.toContain(text.empty)
  await wrapper.get('[data-collection-create-form]').trigger('submit')
  expect(ipc).toHaveBeenCalledExactlyOnceWith('list_project_collections')
  list.resolve([first, second])
  await flushPromises()
  expect(order(wrapper)).toEqual([first.id, second.id])
  expect(wrapper.text()).toContain(text.unnamed)
  expect(wrapper.text()).toContain(second.name)
  expect(wrapper.text()).toContain(projects[0].name)
  expect(wrapper.text()).toContain(projects[0].path)
  expect(wrapper.text()).toContain(text.engines.unreal)
  expect(checkbox(wrapper, projects[0].id).attributes('aria-checked')).toBe('true')
  expect(checkbox(wrapper, projects[1].id).attributes('aria-checked')).toBe('false')
  expect(wrapper.find('img, b, a').exists()).toBe(false)
  await wrapper.setProps({ projects: [projects[1]] })
  expect(wrapper.text()).toContain(text.missingProject.replace('{id}', projects[0].id))
  expect(wrapper.findAll('[role="checkbox"]')).toHaveLength(1)
  expect(ipc).toHaveBeenCalledTimes(1)
  expect(projects).toHaveLength(2)
  expect(wrapper.text()).not.toContain('tauriPreview.projectCollections.')
})

it('creates once with an optional name and renames using the complete returned collection list', async () => {
  const creating = deferredReply()
  const created = { id: 'new-collection', name: null, projectIds: [] }
  const renamed = { ...created, name: 'Host-confirmed name' }
  ipc
    .mockResolvedValueOnce([first, second])
    .mockReturnValueOnce(creating.promise)
    .mockResolvedValueOnce([renamed, second])
  const wrapper = mountPanel()
  await flushPromises()
  await wrapper.get('[data-collection-create-form]').trigger('submit')
  await wrapper.get('[data-collection-create-form]').trigger('submit')
  expect(ipc.mock.calls[1]).toEqual(['create_project_collection', { name: null }])
  expect(ipc).toHaveBeenCalledTimes(2)
  creating.resolve([second, first, created])
  await flushPromises()
  expect(order(wrapper)).toEqual([second.id, first.id, created.id])
  expect(wrapper.get(`[data-collection-select="${created.id}"]`).attributes('aria-pressed')).toBe(
    'true'
  )
  await wrapper.get('[data-collection-rename-form] input').setValue(' Requested name ')
  await wrapper.get('[data-collection-rename-form]').trigger('submit')
  await flushPromises()
  expect(ipc.mock.calls[2]).toEqual([
    'rename_project_collection',
    { collectionId: created.id, name: 'Requested name' }
  ])
  expect(order(wrapper)).toEqual([created.id, second.id])
  expect(wrapper.text()).toContain(renamed.name)
  expect(wrapper.get<HTMLInputElement>('[data-collection-rename-form] input').element.value).toBe(
    renamed.name
  )
  expect(ipc).toHaveBeenCalledTimes(3)
})

it('moves both ways in host order and updates membership by stable registration ID', async () => {
  const moving = deferredReply()
  const added = { ...first, projectIds: [projects[0].id, projects[1].id] }
  const removed = { ...first, projectIds: [projects[1].id] }
  ipc
    .mockResolvedValueOnce([first, second])
    .mockReturnValueOnce(moving.promise)
    .mockResolvedValueOnce([first, second])
    .mockResolvedValueOnce([added, second])
    .mockResolvedValueOnce([removed, second])
  const wrapper = mountPanel()
  await flushPromises()
  expect(wrapper.get(`[data-collection-up="${first.id}"]`).attributes('disabled')).toBeDefined()
  expect(wrapper.get(`[data-collection-down="${second.id}"]`).attributes('disabled')).toBeDefined()
  await wrapper.get(`[data-collection-down="${first.id}"]`).trigger('click')
  await wrapper.get(`[data-collection-down="${first.id}"]`).trigger('click')
  expect(ipc).toHaveBeenCalledTimes(2)
  expect(order(wrapper)).toEqual([first.id, second.id])
  moving.resolve([second, first])
  await flushPromises()
  expect(order(wrapper)).toEqual([second.id, first.id])
  await wrapper.get(`[data-collection-up="${first.id}"]`).trigger('click')
  await flushPromises()
  expect(order(wrapper)).toEqual([first.id, second.id])
  await checkbox(wrapper, projects[1].id).trigger('click')
  await flushPromises()
  expect(checkbox(wrapper, projects[1].id).attributes('aria-checked')).toBe('true')
  await checkbox(wrapper, projects[0].id).trigger('click')
  await flushPromises()
  expect(checkbox(wrapper, projects[0].id).attributes('aria-checked')).toBe('false')
  expect(checkbox(wrapper, projects[1].id).attributes('aria-checked')).toBe('true')
  expect(ipc.mock.calls).toEqual([
    ['list_project_collections'],
    ['move_project_collection', { collectionId: first.id, direction: 'down' }],
    ['move_project_collection', { collectionId: first.id, direction: 'up' }],
    [
      'set_project_collection_membership',
      { collectionId: first.id, registrationId: projects[1].id, belongs: true }
    ],
    [
      'set_project_collection_membership',
      { collectionId: first.id, registrationId: projects[0].id, belongs: false }
    ]
  ])
})

it('requires explicit dissolution approval, keeps state after failure, and never deletes a registration', async () => {
  const removing = deferredReply()
  const text = projectCollectionsTestMessages['en-US'].tauriPreview.projectCollections
  ipc
    .mockResolvedValueOnce([first, second])
    .mockReturnValueOnce(removing.promise)
    .mockResolvedValueOnce([second])
  const wrapper = mountPanel()
  await flushPromises()
  await wrapper.get('[data-collection-dissolve]').trigger('click')
  expect(wrapper.get('[data-collection-confirm]').text()).toContain(
    text.confirmDissolve.replace('{name}', text.unnamed)
  )
  expect(ipc).toHaveBeenCalledTimes(1)
  await wrapper.get('[data-collection-cancel]').trigger('click')
  expect(wrapper.find('[data-collection-confirm]').exists()).toBe(false)
  expect(ipc).toHaveBeenCalledTimes(1)
  await wrapper.get('[data-collection-dissolve]').trigger('click')
  await wrapper.get('[data-collection-approve]').trigger('click')
  await wrapper.get('[data-collection-approve]').trigger('click')
  expect(ipc).toHaveBeenCalledTimes(2)
  removing.reject(new Error('private /fixture/library.sqlite payload'))
  await flushPromises()
  expect(order(wrapper)).toEqual([first.id, second.id])
  expect(wrapper.get('[role="alert"]').text()).toBe(text.changeError)
  expect(wrapper.text()).not.toContain('/fixture/library.sqlite')
  await wrapper.get('[data-collection-approve]').trigger('click')
  await flushPromises()
  expect(order(wrapper)).toEqual([second.id])
  expect(wrapper.find('[data-collection-confirm]').exists()).toBe(false)
  expect(wrapper.find('[role="alert"]').exists()).toBe(false)
  expect(projects).toHaveLength(2)
  expect(ipc.mock.calls).toEqual([
    ['list_project_collections'],
    ['remove_project_collection', { collectionId: first.id }],
    ['remove_project_collection', { collectionId: first.id }]
  ])
})

it('imports an explicitly selected file and reports its missing registrations or authoritative result', async () => {
  ipc
    .mockResolvedValueOnce([])
    .mockRejectedValueOnce('project_collections_import_missing_project')
    .mockResolvedValueOnce({ collections: [first], inserted: 1, alreadyImported: 0 })
  const wrapper = mountPanel()
  await flushPromises()
  const importFile = wrapper.get<HTMLInputElement>('input[type="file"]')
  const manifest = '{"format":"uebox-project-collections"}'
  const file = new File([manifest], 'fixture.json', { type: 'application/json' })
  Object.defineProperty(importFile.element, 'files', { configurable: true, value: [file] })
  await importFile.trigger('change')
  expect(ipc).toHaveBeenCalledExactlyOnceWith('list_project_collections')
  const confirmation = wrapper.get('[data-collection-import]')
  await confirmation.trigger('click')
  await flushPromises()
  expect(ipc).toHaveBeenLastCalledWith('import_project_collections', { manifest })
  expect(wrapper.get('[role="alert"]').text()).toContain(
    'Import the matching project registrations'
  )
  await confirmation.trigger('click')
  await flushPromises()
  expect(order(wrapper)).toEqual([first.id])
  expect(wrapper.text()).toContain('1 imported, 0 already present.')
})
