import { invoke } from '@tauri-apps/api/core'
import { flushPromises, mount, type DOMWrapper, type VueWrapper } from '@vue/test-utils'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import enUS from '@renderer/i18n/locales/en-US'
import zhCN from '@renderer/i18n/locales/zh-CN'
import { createAppI18n, type Locale } from '../i18n'
import type { ModelCatalog, SavedModel } from './commands'
import ModelSettingsPanel from './ModelSettingsPanel.vue'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))

const ipc = vi.mocked(invoke)
const shared = { 'en-US': enUS, 'zh-CN': zhCN }
const labels = enUS.tauriPreview.modelSettings
const existing: SavedModel = {
  id: 'fixture-profile-a',
  title: 'Fixture model A',
  endpoint: 'https://example.invalid/v1/chat/completions',
  model: 'fixture-model-a',
  authorization: 'keychain',
  environment: null
}
const other: SavedModel = {
  ...existing,
  id: 'fixture-profile-b',
  title: 'Fixture model B',
  model: 'fixture-model-b',
  authorization: 'environment',
  environment: 'FIXTURE_MODEL_TOKEN'
}
const empty: ModelCatalog = { profiles: [], bindings: {} }
const catalog: ModelCatalog = { profiles: [existing], bindings: { agent: existing.id } }
const inputSecret = 'fixture-keychain-input-not-a-real-secret'
const responseSecret = 'fixture-keychain-response-not-a-real-secret'
const maxImportBytes = 512 * 1024
let panel: VueWrapper | undefined

beforeEach(() => ipc.mockReset())
afterEach(() => {
  panel?.unmount()
  panel = undefined
})

function mountPanel(locale: Locale = 'en-US'): VueWrapper {
  const i18n = createAppI18n(locale)
  i18n.global.mergeLocaleMessage(locale, { aiProvider: { roles: shared[locale].aiProvider.roles } })
  panel = mount(ModelSettingsPanel, { global: { plugins: [i18n] } })
  return panel
}

function button(
  wrapper: VueWrapper,
  text: string,
  selector = 'button'
): DOMWrapper<HTMLButtonElement> {
  const match = wrapper.findAll<HTMLButtonElement>(selector).find((item) => item.text() === text)
  if (!match) throw new Error(`Missing test button: ${text}`)
  return match
}

function field(wrapper: VueWrapper, text: string): DOMWrapper<Element> {
  const match = wrapper.findAll('label').find((item) => item.text().trim().startsWith(text))
  if (!match) throw new Error(`Missing test field: ${text}`)
  return match
}

async function openPanel(locale: Locale = 'en-US'): Promise<VueWrapper> {
  const wrapper = mountPanel(locale)
  await button(wrapper, shared[locale].tauriPreview.modelSettings.open).trigger('click')
  await flushPromises()
  return wrapper
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

async function fillKeychainProfile(wrapper: VueWrapper): Promise<void> {
  await field(wrapper, labels.name).get('input').setValue(` ${existing.title} `)
  await field(wrapper, labels.endpoint).get('input').setValue(existing.endpoint)
  await field(wrapper, labels.model).get('input').setValue(` ${existing.model} `)
  await field(wrapper, labels.authorization).get('select').setValue('keychain')
  await wrapper.get('input[type="password"]').setValue(inputSecret)
}

function expectNoSecrets(wrapper: VueWrapper): void {
  const inputValues = wrapper.findAll<HTMLInputElement>('input').map((input) => input.element.value)
  for (const secret of [inputSecret, responseSecret]) {
    expect(wrapper.html()).not.toContain(secret)
    expect(inputValues.join('\n')).not.toContain(secret)
  }
}

function archiveFile(content: string | Uint8Array): {
  file: File
  read: ReturnType<typeof vi.fn<() => Promise<ArrayBuffer>>>
} {
  const bytes = typeof content === 'string' ? new TextEncoder().encode(content) : content
  const buffer = new Uint8Array(bytes).buffer
  const file = new File([buffer], 'fixture-model-profiles.json', { type: 'application/json' })
  const read = vi.fn(async () => buffer)
  Object.defineProperty(file, 'arrayBuffer', { value: read })
  return { file, read }
}

async function chooseFile(wrapper: VueWrapper, file: File): Promise<void> {
  const picker = wrapper.get<HTMLInputElement>('input[type="file"]')
  Object.defineProperty(picker.element, 'files', { value: [file], configurable: true })
  await picker.trigger('change')
}

it.each(['en-US', 'zh-CN'] as const)(
  'reads settings only after explicit opening (%s)',
  async (locale) => {
    ipc.mockResolvedValueOnce(empty)
    const wrapper = mountPanel(locale)
    const text = shared[locale].tauriPreview.modelSettings
    await flushPromises()
    expect(ipc).not.toHaveBeenCalled()
    expect(wrapper.text()).toContain(text.title)
    expect(wrapper.find('form').exists()).toBe(false)
    expect(button(wrapper, text.open).attributes('aria-expanded')).toBe('false')

    await button(wrapper, text.open).trigger('click')
    await flushPromises()
    expect(ipc).toHaveBeenCalledExactlyOnceWith('get_model_settings')
    expect(wrapper.text()).toContain(text.empty)
    await button(wrapper, text.close).trigger('click')
    await flushPromises()
    expect(wrapper.find('form').exists()).toBe(false)
    expect(ipc).toHaveBeenCalledTimes(1)
  }
)

it.each(['success', 'rejection'] as const)(
  'sends a Keychain secret once and clears it before %s',
  async (outcome) => {
    const save = deferredReply()
    ipc.mockResolvedValueOnce(empty).mockReturnValueOnce(save.promise)
    const wrapper = await openPanel()
    await fillKeychainProfile(wrapper)
    expect(wrapper.get('input[type="password"]').attributes('autocomplete')).toBe('off')
    await wrapper.get('form').trigger('submit')
    await wrapper.get('form').trigger('submit')
    expect(ipc.mock.calls).toEqual([
      ['get_model_settings'],
      [
        'save_model_profile',
        {
          profile: {
            id: null,
            title: existing.title,
            endpoint: existing.endpoint,
            model: existing.model,
            authorization: { kind: 'keychain', bearer: inputSecret }
          }
        }
      ]
    ])
    expect(wrapper.get<HTMLInputElement>('input[type="password"]').element.value).toBe('')
    expectNoSecrets(wrapper)

    if (outcome === 'success') {
      save.resolve({
        catalog: { ...catalog, profiles: [{ ...existing, bearer: responseSecret }] },
        credentialCleanupPending: false,
        bearer: responseSecret
      })
    } else {
      save.reject(new Error(`${inputSecret} ${responseSecret} /fixture/private-credential`))
    }
    await flushPromises()
    expectNoSecrets(wrapper)
    expect(ipc).toHaveBeenCalledTimes(2)
    if (outcome === 'success') {
      expect(wrapper.get('.model-settings-list').text()).toContain(existing.title)
      expect(wrapper.find('input[type="password"]').exists()).toBe(false)
      expect(wrapper.find('[role="alert"]').exists()).toBe(false)
    } else {
      expect(wrapper.get('[role="alert"]').text()).toBe(labels.errors.generic)
      expect(wrapper.text()).not.toContain('/fixture/private-credential')
      expect(wrapper.get<HTMLInputElement>('input[type="password"]').element.value).toBe('')
    }
  }
)

it('requires a separate removal confirmation, permits cancelling, and deletes only the selected ID', async () => {
  const removal = deferredReply()
  const text = zhCN.tauriPreview.modelSettings
  ipc.mockResolvedValueOnce(catalog).mockReturnValueOnce(removal.promise)
  const wrapper = await openPanel('zh-CN')
  await button(wrapper, text.remove, '.model-settings-list button').trigger('click')
  expect(ipc).toHaveBeenCalledTimes(1)
  expect(wrapper.get('.model-settings-confirm').text()).toContain(
    text.confirmRemove.replace('{title}', existing.title)
  )
  await button(wrapper, text.cancel, '.model-settings-confirm button').trigger('click')
  expect(wrapper.find('.model-settings-confirm').exists()).toBe(false)
  expect(ipc).toHaveBeenCalledTimes(1)

  await button(wrapper, text.remove, '.model-settings-list button').trigger('click')
  await button(wrapper, text.remove, '.model-settings-confirm button').trigger('click')
  await button(wrapper, text.remove, '.model-settings-confirm button').trigger('click')
  expect(ipc.mock.calls[1]).toEqual(['remove_model_profile', { profileId: existing.id }])
  expect(ipc).toHaveBeenCalledTimes(2)
  removal.resolve({ catalog: empty, credentialCleanupPending: false })
  await flushPromises()
  expect(wrapper.find('.model-settings-confirm').exists()).toBe(false)
  expect(wrapper.find('.model-settings-list').exists()).toBe(false)
  expect(wrapper.text()).toContain(text.empty)
})

it.each(['oversized', 'invalidUtf8'] as const)(
  'rejects an %s file without importing it',
  async (kind) => {
    ipc.mockResolvedValueOnce(empty)
    const archive = archiveFile(kind === 'oversized' ? '{}' : new Uint8Array([0xc3, 0x28]))
    if (kind === 'oversized')
      Object.defineProperty(archive.file, 'size', { value: maxImportBytes + 1 })
    const wrapper = await openPanel()
    await chooseFile(wrapper, archive.file)
    expect(archive.read).not.toHaveBeenCalled()
    expect(ipc).toHaveBeenCalledTimes(1)
    await button(wrapper, labels.import).trigger('click')
    await flushPromises()
    expect(ipc).toHaveBeenCalledTimes(1)
    expect(archive.read).toHaveBeenCalledTimes(kind === 'oversized' ? 0 : 1)
    expect(wrapper.get('[role="alert"]').text()).toBe(
      kind === 'oversized'
        ? labels.errors.model_profile_import_limit
        : labels.errors.model_profile_import_invalid
    )
  }
)

it('imports exactly 512 KiB only on click and uses the returned catalog and counts', async () => {
  const json = '{"fixture":"临时模型配置"}'
  const manifest = `${' '.repeat(maxImportBytes - new TextEncoder().encode(json).byteLength)}${json}`
  const archive = archiveFile(manifest)
  const importing = deferredReply()
  ipc.mockResolvedValueOnce(empty).mockReturnValueOnce(importing.promise)
  const wrapper = await openPanel()
  expect(button(wrapper, labels.import).attributes('disabled')).toBeDefined()
  await chooseFile(wrapper, archive.file)
  expect(archive.file.size).toBe(maxImportBytes)
  expect(archive.read).not.toHaveBeenCalled()
  expect(ipc).toHaveBeenCalledTimes(1)
  await button(wrapper, labels.import).trigger('click')
  await button(wrapper, labels.import).trigger('click')
  await flushPromises()
  expect(archive.read).toHaveBeenCalledTimes(1)
  expect(ipc.mock.calls[1]).toEqual(['import_model_profiles', { manifest }])
  expect(ipc).toHaveBeenCalledTimes(2)
  importing.resolve({ catalog, inserted: 1, alreadyImported: 2, needsAuthorization: 3 })
  await flushPromises()
  expect(wrapper.get('.model-settings-list').text()).toContain(existing.title)
  expect(wrapper.get('[role="status"]').text()).toBe(
    labels.imported
      .replace('{inserted}', '1')
      .replace('{alreadyImported}', '2')
      .replace('{needsAuthorization}', '3')
  )
  expect(button(wrapper, labels.import).attributes('disabled')).toBeDefined()
  expect(wrapper.get<HTMLInputElement>('input[type="file"]').element.value).toBe('')
  expect(field(wrapper, labels.binding).get<HTMLSelectElement>('select').element.value).toBe(
    existing.id
  )
})

it('selects defaults by purpose and binds or clears only the chosen purpose using host results', async () => {
  const initial: ModelCatalog = {
    profiles: [existing, other],
    bindings: { agent: existing.id, chat: other.id }
  }
  const bound: ModelCatalog = { ...initial, bindings: { agent: existing.id, chat: existing.id } }
  const unbound: ModelCatalog = { ...initial, bindings: { chat: existing.id } }
  ipc.mockResolvedValueOnce(initial).mockResolvedValueOnce(bound).mockResolvedValueOnce(unbound)
  const wrapper = await openPanel()
  const purpose = field(wrapper, labels.purpose).get<HTMLSelectElement>('select')
  const binding = field(wrapper, labels.binding).get<HTMLSelectElement>('select')
  expect(purpose.element.value).toBe('agent')
  expect(binding.element.value).toBe(existing.id)
  await purpose.setValue('chat')
  expect(binding.element.value).toBe(other.id)
  expect(ipc).toHaveBeenCalledTimes(1)
  await binding.setValue(existing.id)
  await flushPromises()
  expect(ipc.mock.calls[1]).toEqual([
    'bind_model_profile',
    { purpose: 'chat', profileId: existing.id }
  ])
  expect(binding.element.value).toBe(existing.id)

  await purpose.setValue('agent')
  expect(binding.element.value).toBe(existing.id)
  await binding.setValue('')
  await flushPromises()
  expect(ipc.mock.calls[2]).toEqual(['bind_model_profile', { purpose: 'agent', profileId: null }])
  expect(binding.element.value).toBe('')
  await purpose.setValue('chat')
  expect(binding.element.value).toBe(existing.id)
  await purpose.setValue('agent')
  expect(binding.element.value).toBe('')
  expect(ipc).toHaveBeenCalledTimes(3)
})
