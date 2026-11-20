import { invoke } from '@tauri-apps/api/core'
import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { nextTick } from 'vue'
import { createAppI18n } from '../i18n'
import ProjectLibraryPanel from './ProjectLibraryPanel.vue'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))

const ipc = vi.mocked(invoke)
const existing = { id: '9007199254740993', engine: 'unity', name: 'Fixture A', path: '/tmp/A' }
const other = { ...existing, id: '9007199254740992', engine: 'unreal' }
const created = { id: '9007199254740994', engine: 'godot', name: 'Fixture B', path: '/tmp/B' }
let panel: { unmount: () => void } | undefined

beforeEach(() => ipc.mockReset())
afterEach(() => panel?.unmount())

function openPanel(): VueWrapper {
  const wrapper = mount(ProjectLibraryPanel, { global: { plugins: [createAppI18n('en-US')] } })
  panel = wrapper
  return wrapper
}

function deferredReply(): { promise: Promise<unknown>; resolve: (value: unknown) => void } {
  let resolve!: (value: unknown) => void
  const promise = new Promise((done) => {
    resolve = done
  })
  return { promise, resolve }
}

it('imports the chosen file on confirmation and reloads persisted registrations', async () => {
  const manifest = '{"format":"uebox-project-registration","version":1}'
  const file = new File([manifest], 'legacy-projects.json', { type: 'application/json' })
  const read = vi.fn(async () => new TextEncoder().encode(manifest).buffer)
  Object.defineProperty(file, 'arrayBuffer', { value: read })
  ipc
    .mockResolvedValueOnce([])
    .mockResolvedValueOnce({ inserted: 1, alreadyRegistered: 2, duplicateInFile: 3 })
    .mockResolvedValueOnce([created])
  const wrapper = openPanel()
  await flushPromises()

  expect(wrapper.get('[data-project-import]').attributes('disabled')).toBeDefined()
  const picker = wrapper.get('input[type="file"]')
  Object.defineProperty(picker.element, 'files', { value: [file] })
  await picker.trigger('change')
  expect(read).not.toHaveBeenCalled()
  expect(ipc).toHaveBeenCalledTimes(1)
  await wrapper.get('[data-project-import]').trigger('click')
  await flushPromises()

  expect(ipc.mock.calls).toEqual([
    ['list_registered_projects'],
    ['import_registered_projects', { manifest }],
    ['list_registered_projects']
  ])
  expect(wrapper.text()).toContain('Fixture B')
  expect(wrapper.text()).toContain('1 added')
  expect(wrapper.text()).toContain('2 already registered')
  expect(wrapper.text()).toContain('3 duplicate entries in the file')
})

it('blocks duplicate reads and writes during the initial list request', async () => {
  const list = deferredReply()
  ipc.mockReturnValueOnce(list.promise)
  const wrapper = openPanel()
  await nextTick()
  expect(wrapper.text()).toContain('Loading registrations')
  expect(wrapper.get('[data-project-refresh]').attributes('disabled')).toBeDefined()
  await wrapper.get('[data-project-refresh]').trigger('click')
  await wrapper.get('form').trigger('submit')
  expect(ipc).toHaveBeenCalledExactlyOnceWith('list_registered_projects')

  list.resolve([])
  await flushPromises()
  expect(wrapper.text()).toContain('No projects registered')
})

it('lets the user retry a failed initial read and shows an empty state only after success', async () => {
  ipc.mockRejectedValueOnce('project_library_recovery_required').mockResolvedValueOnce([])
  const wrapper = openPanel()
  await flushPromises()
  expect(wrapper.text()).toContain('needs recovery')
  expect(wrapper.text()).not.toContain('No projects registered')

  await wrapper.get('[data-project-refresh]').trigger('click')
  await flushPromises()
  expect(wrapper.text()).toContain('No projects registered')
  expect(wrapper.text()).not.toContain('needs recovery')
})

it('keeps the list after a failed refresh and lets the user retry', async () => {
  ipc.mockResolvedValueOnce([existing]).mockRejectedValueOnce('project_library_locked')
  const wrapper = openPanel()
  await flushPromises()
  await wrapper.get('[data-project-refresh]').trigger('click')
  await flushPromises()
  expect(wrapper.text()).toContain('Fixture A')
  expect(wrapper.text()).toContain('The project registry is busy')

  ipc.mockResolvedValueOnce([created])
  await wrapper.get('[data-project-refresh]').trigger('click')
  await flushPromises()
  expect(wrapper.text()).toContain('Fixture B')
  expect(wrapper.text()).not.toContain('Fixture A')
  expect(wrapper.text()).not.toContain('The project registry is busy')
})

it('uses the returned registration and clears the form without listing again', async () => {
  const save = deferredReply()
  ipc.mockResolvedValueOnce([existing]).mockReturnValueOnce(save.promise)
  const wrapper = openPanel()
  await flushPromises()
  const engine = wrapper.get<HTMLSelectElement>('select[name="project-engine"]')
  const name = wrapper.get<HTMLInputElement>('input[name="project-name"]')
  const path = wrapper.get<HTMLInputElement>('input[name="project-path"]')
  await engine.setValue('godot')
  await name.setValue('Entered name')
  await path.setValue('/tmp/B')
  await wrapper.get('form').trigger('submit')
  await wrapper.get('form').trigger('submit')
  await wrapper.get('[data-project-refresh]').trigger('click')
  await wrapper.get('[data-project-remove]').trigger('click')
  expect(ipc.mock.calls).toEqual([
    ['list_registered_projects'],
    ['register_project', { engine: 'godot', name: 'Entered name', path: '/tmp/B' }]
  ])

  save.resolve(created)
  await flushPromises()
  expect(wrapper.text()).toContain('Fixture A')
  expect(wrapper.text()).toContain('Fixture B')
  expect(engine.element.value).toBe('')
  expect(name.element.value).toBe('')
  expect(path.element.value).toBe('')
  expect(ipc).toHaveBeenCalledTimes(2)
})

it('preserves the registration fields on rejection and permits another submission', async () => {
  ipc.mockResolvedValueOnce([]).mockRejectedValueOnce('project_duplicate')
  const wrapper = openPanel()
  await flushPromises()
  const engine = wrapper.get<HTMLSelectElement>('select[name="project-engine"]')
  const name = wrapper.get<HTMLInputElement>('input[name="project-name"]')
  const path = wrapper.get<HTMLInputElement>('input[name="project-path"]')
  await engine.setValue('godot')
  await name.setValue(created.name)
  await path.setValue(created.path)
  await wrapper.get('form').trigger('submit')
  await flushPromises()
  expect(wrapper.text()).toContain('already registered for this engine')
  expect(engine.element.value).toBe('godot')
  expect(name.element.value).toBe(created.name)
  expect(path.element.value).toBe(created.path)

  ipc.mockResolvedValueOnce(created)
  await wrapper.get('form').trigger('submit')
  await flushPromises()
  expect(wrapper.text()).toContain(created.name)
  expect(wrapper.text()).not.toContain('already registered for this engine')
})

it.each([true, false])(
  'removes only the exact ID after a resolved removal (%s)',
  async (removed) => {
    const removal = deferredReply()
    ipc.mockResolvedValueOnce([existing, other]).mockReturnValueOnce(removal.promise)
    const wrapper = openPanel()
    await flushPromises()
    await wrapper.get('[data-project-remove]').trigger('click')
    await wrapper.get('[data-project-refresh]').trigger('click')
    expect(ipc.mock.calls).toEqual([
      ['list_registered_projects'],
      ['remove_registered_project', { id: existing.id }]
    ])
    expect(wrapper.findAll('[data-project-remove]')).toHaveLength(2)
    removal.resolve(removed)
    await flushPromises()
    expect(wrapper.findAll('[data-project-remove]')).toHaveLength(1)
    expect(wrapper.text()).toContain('Unreal Engine')
    expect(wrapper.get('li').text()).not.toContain('Unity')
    expect(ipc).toHaveBeenCalledTimes(2)
  }
)

it('keeps the row on a removal error and displays a generic message for unknown errors', async () => {
  ipc.mockResolvedValueOnce([existing]).mockRejectedValueOnce('unexpected /tmp/private')
  const wrapper = openPanel()
  await flushPromises()
  await wrapper.get('[data-project-remove]').trigger('click')
  await flushPromises()
  expect(wrapper.text()).toContain('Fixture A')
  expect(wrapper.text()).toContain('Unable to complete the project registration operation')
  expect(wrapper.text()).not.toContain('/tmp/private')
})
