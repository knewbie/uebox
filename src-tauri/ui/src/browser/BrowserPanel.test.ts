import { invoke } from '@tauri-apps/api/core'
import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { createAppI18n, type Locale } from '../i18n'
import { browserTestMessages } from './messages.fixture'
import BrowserPanel from './BrowserPanel.vue'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))

const closed = {
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
const opened = {
  ...closed,
  sessionId: '7',
  open: true,
  address: 'https://fixture.example/one',
  origin: 'https://fixture.example',
  canGoBack: true
}
let wrapper: VueWrapper | undefined

function render(locale: Locale = 'en-US'): VueWrapper {
  const i18n = createAppI18n(locale)
  for (const language of ['en-US', 'zh-CN'] as const) {
    i18n.global.mergeLocaleMessage(language, browserTestMessages[language])
  }
  wrapper = mount(BrowserPanel, { global: { plugins: [i18n] } })
  return wrapper
}

beforeEach(() => {
  vi.useFakeTimers()
  vi.mocked(invoke).mockReset()
})
afterEach(() => {
  wrapper?.unmount()
  wrapper = undefined
  vi.useRealTimers()
})

it('reads status without opening a website and opens only the explicitly submitted HTTPS address', async () => {
  const ipc = vi.mocked(invoke).mockResolvedValueOnce(closed).mockResolvedValueOnce(opened)
  const view = render()
  await flushPromises()
  expect(ipc.mock.calls).toEqual([['isolated_browser_status']])
  expect(view.text()).toContain('Browser window is closed.')
  expect(view.get('[data-browser-back]').attributes('disabled')).toBeDefined()
  await view.get('#isolated-browser-url').setValue(' https://fixture.example/one ')
  expect(ipc).toHaveBeenCalledTimes(1)
  await view.get('form').trigger('submit')
  await flushPromises()
  expect(ipc.mock.calls[1]).toEqual([
    'open_browser_window',
    { address: 'https://fixture.example/one' }
  ])
  expect(view.text()).toContain('https://fixture.example/one')
  expect(view.text()).toContain('Browser window is open.')
  expect(view.get('[data-browser-open]').attributes('disabled')).toBeDefined()
})

it('binds manual history and reload to the current session and disables unavailable directions', async () => {
  const ipc = vi.mocked(invoke).mockResolvedValueOnce(opened)
  const view = render('zh-CN')
  await flushPromises()
  expect(view.get('[data-browser-forward]').attributes('disabled')).toBeDefined()
  ipc.mockResolvedValueOnce({
    ...opened,
    canGoBack: false,
    canGoForward: true,
    address: 'https://fixture.example/zero'
  })
  await view.get('[data-browser-back]').trigger('click')
  await flushPromises()
  expect(ipc.mock.calls[1]).toEqual([
    'control_isolated_browser',
    { sessionId: '7', action: 'back' }
  ])
  expect(view.get('[data-browser-back]').attributes('disabled')).toBeDefined()
  ipc.mockResolvedValueOnce(opened)
  await view.get('[data-browser-forward]').trigger('click')
  await flushPromises()
  expect(ipc.mock.calls[2]).toEqual([
    'control_isolated_browser',
    { sessionId: '7', action: 'forward' }
  ])
  ipc.mockResolvedValueOnce({ ...opened, loading: true })
  await view.get('[data-browser-reload]').trigger('click')
  await flushPromises()
  expect(ipc.mock.calls[3]).toEqual([
    'control_isolated_browser',
    { sessionId: '7', action: 'reload' }
  ])
  expect(view.text()).toContain('正在加载页面…')
})

it('ignores a stale status read after a user opens a new window and stops polling after unmount', async () => {
  let finishRead!: (value: typeof closed) => void
  const ipc = vi.mocked(invoke).mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finishRead = resolve
      })
  )
  const view = render()
  ipc.mockResolvedValueOnce(opened)
  await view.get('#isolated-browser-url').setValue('https://fixture.example/one')
  await view.get('form').trigger('submit')
  await flushPromises()
  finishRead(closed)
  await flushPromises()
  expect(view.text()).toContain('https://fixture.example/one')
  ipc.mockResolvedValueOnce(closed)
  await vi.advanceTimersByTimeAsync(1000)
  await flushPromises()
  expect(view.text()).toContain('Browser window is closed.')
  const count = ipc.mock.calls.length
  view.unmount()
  wrapper = undefined
  await vi.advanceTimersByTimeAsync(3000)
  expect(ipc).toHaveBeenCalledTimes(count)
})

it('serializes user controls and never renders rejected IPC payloads as page content', async () => {
  const ipc = vi.mocked(invoke).mockResolvedValueOnce(opened)
  const view = render()
  await flushPromises()
  let fail!: (error: unknown) => void
  ipc.mockImplementationOnce(
    () =>
      new Promise((_resolve, reject) => {
        fail = reject
      })
  )
  await view.get('[data-browser-back]').trigger('click')
  await view.get('[data-browser-reload]').trigger('click')
  expect(ipc).toHaveBeenCalledTimes(2)
  fail('<img src=x onerror=alert(1)> /private/profile/secret')
  await flushPromises()
  expect(view.text()).toContain('Unable to control the browser. Try again.')
  expect(view.text()).not.toContain('/private/profile/secret')
  expect(view.find('img').exists()).toBe(false)
  expect(view.text()).toContain('https://fixture.example/one')
})

it('shows blocked navigation and downloads as text with no download action or unsafe page links', async () => {
  vi.mocked(invoke).mockResolvedValueOnce({
    ...opened,
    address: 'https://fixture.example/?q=<img src=x onerror=alert(1)>',
    navigationBlocked: true,
    downloadBlocked: true,
    historySupported: false
  })
  const view = render()
  await flushPromises()
  expect(view.text()).toContain('A navigation outside the allowed HTTPS origin was blocked.')
  expect(view.text()).toContain('A download request was blocked. No file was saved.')
  expect(view.text()).toContain('Native back and forward controls are unavailable')
  expect(view.get('[data-browser-back]').attributes('disabled')).toBeDefined()
  expect(view.get('[data-browser-forward]').attributes('disabled')).toBeDefined()
  expect(view.get('[data-browser-reload]').attributes('disabled')).toBeUndefined()
  expect(view.findAll('a, iframe, img').length).toBe(0)
  expect(view.find('[data-browser-download]').exists()).toBe(false)
})

it('reports status failures and a replaced session without displaying raw errors or changing the draft', async () => {
  const ipc = vi.mocked(invoke).mockRejectedValueOnce('sensitive native error')
  const view = render()
  await flushPromises()
  expect(view.text()).toContain('Unable to read browser status. Check again.')
  await view.get('#isolated-browser-url').setValue('https://draft.example/')
  ipc.mockResolvedValueOnce(opened)
  await view.get('[data-browser-check]').trigger('click')
  await flushPromises()
  expect(view.text()).not.toContain('Unable to read browser status.')
  expect((view.get('#isolated-browser-url').element as HTMLInputElement).value).toBe(
    'https://draft.example/'
  )
  ipc.mockRejectedValueOnce('browser_invalid_session')
  await view.get('[data-browser-back]').trigger('click')
  await flushPromises()
  expect(view.text()).toContain('This browser window has changed.')
  expect(view.text()).not.toContain('sensitive native error')
})

it('keeps controls disabled while a window opens and displays only the subsequently observed address', async () => {
  const ipc = vi
    .mocked(invoke)
    .mockResolvedValueOnce({ ...opened, opening: true, loading: true, address: null })
  const view = render()
  await flushPromises()
  expect(view.text()).toContain('Opening browser window…')
  expect(view.text()).not.toContain('https://fixture.example/one')
  expect(view.get('[data-browser-reload]').attributes('disabled')).toBeDefined()
  ipc.mockResolvedValueOnce(opened)
  await vi.advanceTimersByTimeAsync(1000)
  await flushPromises()
  expect(view.text()).toContain('https://fixture.example/one')
  expect(view.get('[data-browser-reload]').attributes('disabled')).toBeUndefined()
})

it('fails closed on a malformed status instead of enabling controls or rendering its payload', async () => {
  vi.mocked(invoke).mockResolvedValueOnce({
    ...opened,
    open: false,
    downloadsEnabled: true,
    address: '<script>sensitive-payload</script>'
  })
  const view = render()
  await flushPromises()
  expect(view.text()).toContain('Unable to read browser status. Check again.')
  expect(view.text()).not.toContain('sensitive-payload')
  expect(view.get('[data-browser-back]').attributes('disabled')).toBeDefined()
  expect(view.get('[data-browser-reload]').attributes('disabled')).toBeDefined()
})

it('saves a link only after explicit submission from the active session', async () => {
  const ipc = vi.mocked(invoke).mockResolvedValueOnce({ ...opened, downloadsEnabled: true })
  const view = render()
  await flushPromises()
  await view.get('#browser-download-url').setValue('https://fixture.example/report.txt')
  expect(ipc).toHaveBeenCalledTimes(1)
  ipc.mockResolvedValueOnce(true)
  await view.get('[data-browser-save-link]').trigger('submit')
  await flushPromises()
  expect(ipc).toHaveBeenLastCalledWith('save_browser_link', {
    sessionId: opened.sessionId,
    address: 'https://fixture.example/report.txt'
  })
  expect(view.get('[data-browser-save-link] [role="status"]').text()).toContain('File saved.')
})
