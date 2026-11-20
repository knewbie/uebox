import { beforeEach, expect, it, vi } from 'vitest'
import { join, resolve } from 'node:path'
import { registerChatHistoryIPC } from './chatHistory'

const fixture = vi.hoisted(() => ({
  getPath: vi.fn(),
  handle: vi.fn(),
  fromWebContents: vi.fn(),
  showSaveDialog: vi.fn(),
  exportHistory: vi.fn(),
  on: vi.fn()
}))

vi.mock('electron', () => ({
  app: { getPath: fixture.getPath },
  BrowserWindow: { fromWebContents: fixture.fromWebContents },
  dialog: { showSaveDialog: fixture.showSaveDialog },
  ipcMain: { handle: fixture.handle, on: fixture.on }
}))

vi.mock('./chatHistoryExport', () => ({
  exportVisibleChatHistory: fixture.exportHistory
}))

const userData = resolve('fixture-electron-profile')
const destination = resolve('fixture-export/visible-history.json')
const sender = { isDestroyed: vi.fn() }
const window = { fixture: true }

type Handler = (event: { sender: typeof sender }, ignored?: string) => Promise<unknown>

function invokeExport(ignored?: string): Promise<unknown> {
  const handler = fixture.handle.mock.calls.find(([name]) => name === 'chat-history:export')?.[1]
  expect(handler).toBeTypeOf('function')
  return (handler as Handler)({ sender }, ignored)
}

beforeEach(() => {
  vi.resetAllMocks()
  fixture.getPath.mockReturnValue(userData)
  fixture.fromWebContents.mockReturnValue(window)
  fixture.exportHistory.mockResolvedValue({
    success: true,
    code: 'chat_history_exported',
    count: 2
  })
  registerChatHistoryIPC()
})

it('exports only after native selection and derives the source from the Electron profile', async () => {
  fixture.showSaveDialog.mockResolvedValue({ canceled: false, filePath: destination })

  await expect(invokeExport('untrusted renderer path')).resolves.toMatchObject({
    success: true,
    count: 2
  })
  expect(fixture.showSaveDialog).toHaveBeenCalledWith(
    window,
    expect.objectContaining({ properties: ['dontAddToRecent'] })
  )
  expect(fixture.exportHistory).toHaveBeenCalledExactlyOnceWith({
    historyDir: join(userData, 'chat-history'),
    destination
  })
})

it('leaves the source unread on cancellation and blocks reentry while choosing a location', async () => {
  let close: ((value: { canceled: boolean }) => void) | undefined
  fixture.showSaveDialog.mockImplementationOnce(
    () => new Promise<{ canceled: boolean }>((done) => (close = done))
  )
  const pending = invokeExport()
  await expect(invokeExport()).resolves.toEqual({
    success: false,
    code: 'chat_history_export_busy'
  })
  close?.({ canceled: true })
  await expect(pending).resolves.toEqual({ success: false, code: 'chat_history_export_cancelled' })
  expect(fixture.exportHistory).not.toHaveBeenCalled()
})

it('avoids writing after the requesting window is destroyed', async () => {
  fixture.showSaveDialog.mockResolvedValue({ canceled: false, filePath: destination })
  sender.isDestroyed.mockReturnValue(true)

  await expect(invokeExport()).resolves.toMatchObject({ code: 'chat_history_export_cancelled' })
  expect(fixture.exportHistory).not.toHaveBeenCalled()
})
