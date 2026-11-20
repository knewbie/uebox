import { invoke } from '@tauri-apps/api/core'
import { flushPromises, mount } from '@vue/test-utils'
import { expect, it, vi } from 'vitest'
import { createAppI18n } from '../i18n'
import AssetIndexPanel from './AssetIndexPanel.vue'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))

it('keeps identically named libraries separate and searches only the selected index', async () => {
  const ipc = vi.mocked(invoke)
  const empty = { items: [], nextId: null }
  const vaults = [
    { identity: 'a', name: 'Shared', rootPath: '/fixture/a', indexAvailable: true },
    { identity: 'b', name: 'Shared', rootPath: '/fixture/b', indexAvailable: true }
  ]
  const candidate = {
    identity: 'hero',
    name: 'Fixture asset',
    relativePath: 'assets/hero.tscn',
    size: 6,
    tags: ['fixture']
  }
  let resolveA!: (value: unknown) => void
  const delayedFolders = new Promise((resolve) => {
    resolveA = resolve
  })
  ipc
    .mockReset()
    .mockResolvedValueOnce({ items: vaults, nextId: null })
    .mockReturnValueOnce(delayedFolders)
    .mockResolvedValueOnce(empty)
    .mockResolvedValueOnce(empty)
    .mockResolvedValueOnce({ items: [candidate], nextId: null })
    .mockResolvedValueOnce({ items: [candidate], nextId: null })

  const wrapper = mount(AssetIndexPanel, { global: { plugins: [createAppI18n('en-US')] } })
  try {
    await flushPromises()
    expect(wrapper.get('#asset-index-vault').text()).toContain('/fixture/a')
    expect(wrapper.get('#asset-index-vault').text()).toContain('/fixture/b')
    await wrapper.get('#asset-index-vault').setValue('a')
    await wrapper.get('#asset-index-vault').setValue('b')
    await flushPromises()
    resolveA({ items: [{ identity: 'private-a', name: 'Only in A' }], nextId: null })
    await flushPromises()
    expect(wrapper.text()).not.toContain('Only in A')
    expect(wrapper.text()).toContain(candidate.relativePath)
    await wrapper.get('#asset-index-search').setValue('hero')
    await wrapper.get('form').trigger('submit')
    await flushPromises()
    expect(ipc.mock.calls.at(-1)).toEqual([
      'list_asset_items',
      {
        vaultId: 'b',
        folderId: null,
        afterId: null,
        limit: 25,
        search: 'hero'
      }
    ])
  } finally {
    wrapper.unmount()
  }
})
