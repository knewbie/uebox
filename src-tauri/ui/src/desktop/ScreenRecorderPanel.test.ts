import { invoke } from '@tauri-apps/api/core'
import { flushPromises, mount } from '@vue/test-utils'
import { expect, it, vi } from 'vitest'
import { createAppI18n } from '../i18n'
import ScreenRecorderPanel from './ScreenRecorderPanel.vue'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))

const idle = {
  supported: true,
  token: null,
  phase: 'idle',
  audio: false,
  fileName: null,
  error: null,
  cleanupPending: false
}

it('requests the system picker only on click and cancels using the exact native token', async () => {
  const ipc = vi.mocked(invoke)
  ipc
    .mockReset()
    .mockResolvedValueOnce(idle)
    .mockResolvedValueOnce({ ...idle, token: '11', phase: 'selecting', audio: true })
    .mockResolvedValueOnce({ ...idle, token: '11', phase: 'cancelled', audio: true })
  const wrapper = mount(ScreenRecorderPanel, { global: { plugins: [createAppI18n('en-US')] } })
  try {
    await flushPromises()
    expect(ipc.mock.calls).toEqual([['screen_recording_status']])
    await wrapper.get('[role="checkbox"]').trigger('click')
    await wrapper.get('[data-record-start]').trigger('click')
    await flushPromises()
    expect(ipc.mock.calls[1]).toEqual(['start_screen_recording', { optIn: true, audio: true }])
    expect(wrapper.text()).toContain('Waiting for a selection in the system picker')
    expect(wrapper.find('[data-record-start]').exists()).toBe(false)
    await wrapper.get('[data-record-stop]').trigger('click')
    await flushPromises()
    expect(ipc.mock.calls[2]).toEqual(['stop_screen_recording', { token: '11' }])
    expect(wrapper.text()).toContain('Recording cancelled.')
  } finally {
    wrapper.unmount()
  }
})
