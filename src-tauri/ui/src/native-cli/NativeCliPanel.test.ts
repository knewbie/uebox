import { invoke } from '@tauri-apps/api/core'
import { flushPromises, mount } from '@vue/test-utils'
import { expect, it, vi } from 'vitest'
import { createAppI18n } from '../i18n'
import NativeCliPanel from './NativeCliPanel.vue'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))

it('shows the native endpoint only after explicit start and stops it from the same panel', async () => {
  const rpc = vi.mocked(invoke)
  rpc
    .mockResolvedValueOnce(null)
    .mockResolvedValueOnce({ endpoint: '/private/fixture runtime/cli' })
    .mockResolvedValueOnce(undefined)
  const wrapper = mount(NativeCliPanel, { global: { plugins: [createAppI18n('en-US')] } })
  await flushPromises()
  expect(rpc).toHaveBeenCalledExactlyOnceWith('native_cli_endpoint')
  await wrapper.get('button').trigger('click')
  await flushPromises()
  expect(rpc).toHaveBeenLastCalledWith('start_native_cli')
  expect(wrapper.get('code').text()).toBe('/private/fixture runtime/cli')
  await wrapper.get('button').trigger('click')
  await flushPromises()
  expect(rpc).toHaveBeenLastCalledWith('stop_native_cli')
  expect(wrapper.find('code').exists()).toBe(false)
  wrapper.unmount()
})
