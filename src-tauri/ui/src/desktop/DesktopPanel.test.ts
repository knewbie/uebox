import { invoke } from '@tauri-apps/api/core'
import { flushPromises, mount } from '@vue/test-utils'
import { expect, it, vi } from 'vitest'
import { createAppI18n } from '../i18n'
import DesktopPanel from './DesktopPanel.vue'
import { browserTestMessages } from '../browser/messages.fixture'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))

it('leaves the system tray off until the user enables it and retains the state after a failed change', async () => {
  const ipc = vi.mocked(invoke)
  ipc
    .mockReset()
    .mockResolvedValueOnce(false)
    .mockResolvedValueOnce(true)
    .mockRejectedValueOnce('desktop_tray_unavailable')
  const wrapper = mount(DesktopPanel, {
    global: {
      plugins: [createAppI18n('zh-CN')],
      stubs: { ScreenRecorderPanel: true, BrowserPanel: true }
    }
  })
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

it('opens a browser only after a URL is submitted and shows a stable error on a failed request', async () => {
  const ipc = vi.mocked(invoke)
  ipc.mockReset().mockImplementation(async (command) => {
    if (command === 'desktop_tray') return false
    if (command === 'isolated_browser_status')
      return {
        sessionId: null,
        open: false,
        opening: false,
        address: null,
        origin: null,
        loading: false,
        canGoBack: false,
        canGoForward: false,
        historySupported: true,
        navigationBlocked: false,
        downloadBlocked: false,
        downloadsEnabled: false
      }
    throw 'browser_already_open'
  })
  const i18n = createAppI18n('en-US')
  i18n.global.mergeLocaleMessage('en-US', browserTestMessages['en-US'])
  const wrapper = mount(DesktopPanel, {
    global: { plugins: [i18n], stubs: { ScreenRecorderPanel: true } }
  })
  try {
    await flushPromises()
    await wrapper.get('#isolated-browser-url').setValue('https://fixture.example/page')
    expect(ipc.mock.calls).toHaveLength(2)
    expect(ipc.mock.calls).toContainEqual(['desktop_tray'])
    expect(ipc.mock.calls).toContainEqual(['isolated_browser_status'])
    await wrapper.get('form').trigger('submit')
    await flushPromises()
    expect(ipc.mock.calls[2]).toEqual([
      'open_browser_window',
      {
        address: 'https://fixture.example/page'
      }
    ])
    expect(wrapper.text()).toContain('Close the current browser window before opening a new one.')
  } finally {
    wrapper.unmount()
  }
})
