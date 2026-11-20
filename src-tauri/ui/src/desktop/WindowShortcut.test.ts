import { invoke } from '@tauri-apps/api/core'
import { flushPromises, mount } from '@vue/test-utils'
import { expect, it, vi } from 'vitest'
import { createAppI18n } from '../i18n'
import WindowShortcut from './WindowShortcut.vue'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))

it('registers only on explicit apply and keeps the active binding on failure', async () => {
  const ipc = vi.mocked(invoke)
  const active = { control: true, option: true, shift: false, command: false, key: 'KeyK' }
  ipc
    .mockReset()
    .mockResolvedValueOnce(null)
    .mockResolvedValueOnce(active)
    .mockRejectedValueOnce('desktop_shortcut_occupied')
    .mockResolvedValueOnce(null)
  const wrapper = mount(WindowShortcut, { global: { plugins: [createAppI18n('en-US')] } })
  try {
    await flushPromises()
    const input = wrapper.get('#desktop-window-shortcut')
    await input.trigger('keydown', { key: 'k', code: 'KeyK' })
    expect(wrapper.text()).toContain('Use Ctrl, Alt, or Command')
    await input.trigger('keydown', { key: 'k', code: 'KeyK', ctrlKey: true, altKey: true })
    expect(ipc).toHaveBeenCalledExactlyOnceWith('desktop_window_shortcut')
    await wrapper.get('button').trigger('click')
    await flushPromises()
    expect(ipc).toHaveBeenLastCalledWith('set_desktop_window_shortcut', { shortcut: active })
    expect(wrapper.get('[role="status"]').text()).toContain('Alt / Option + K')
    await input.trigger('keydown', { key: 'l', code: 'KeyL', ctrlKey: true, altKey: true })
    await wrapper.get('button').trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('The system could not register')
    expect(wrapper.get('[role="status"]').text()).toContain('Alt / Option + K')
    await wrapper.findAll('button')[1].trigger('click')
    await flushPromises()
    expect(ipc).toHaveBeenLastCalledWith('set_desktop_window_shortcut', { shortcut: null })
    expect(wrapper.find('[role="status"]').exists()).toBe(false)
  } finally {
    wrapper.unmount()
  }
})
