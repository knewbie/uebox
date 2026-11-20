import { invoke } from '@tauri-apps/api/core'
import { flushPromises, mount } from '@vue/test-utils'
import { expect, it, vi } from 'vitest'
import { createAppI18n } from '../i18n'
import DesktopPanel from './DesktopPanel.vue'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))

it('leaves the system tray off until the user enables it and retains the state after a failed change', async () => {
  const ipc = vi.mocked(invoke)
  ipc
    .mockReset()
    .mockResolvedValueOnce(false)
    .mockResolvedValueOnce(true)
    .mockRejectedValueOnce('desktop_tray_unavailable')
  const wrapper = mount(DesktopPanel, { global: { plugins: [createAppI18n('zh-CN')] } })
  try {
    await flushPromises()
    expect(ipc.mock.calls).toEqual([['desktop_tray']])
    const button = wrapper.get('button')
    expect(button.text()).toBe('启用系统托盘')
    await button.trigger('click')
    await flushPromises()
    expect(ipc.mock.calls[1]).toEqual(['set_desktop_tray', { enabled: true, language: 'zh-CN' }])
    expect(button.attributes('aria-pressed')).toBe('true')
    await button.trigger('click')
    await flushPromises()
    expect(ipc.mock.calls[2]).toEqual(['set_desktop_tray', { enabled: false, language: 'zh-CN' }])
    expect(button.text()).toBe('停用系统托盘')
    expect(wrapper.text()).toContain('无法更改系统托盘设置。')
  } finally {
    wrapper.unmount()
  }
})
