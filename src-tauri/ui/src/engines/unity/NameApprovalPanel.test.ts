import { invoke } from '@tauri-apps/api/core'
import { flushPromises, mount } from '@vue/test-utils'
import { expect, it, vi } from 'vitest'
import { createAppI18n } from '../../i18n'
import NameApprovalPanel from './NameApprovalPanel.vue'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))

it('waits for an explicit answer and renders the editor read-back', async () => {
  const ipc = vi.mocked(invoke)
  ipc
    .mockReset()
    .mockResolvedValueOnce({
      approvals: [
        {
          approvalId: 'fixture-approval',
          sceneHandle: 123,
          gameObjectInstanceId: -321,
          oldName: 'Original object',
          newName: 'New object'
        }
      ],
      receipts: []
    })
    .mockResolvedValueOnce(undefined)
    .mockResolvedValue({
      approvals: [],
      receipts: [{ approvalId: 'fixture-approval', status: 'applied', actualName: 'New object' }]
    })
  const wrapper = mount(NameApprovalPanel, {
    props: { agentId: 'fixture-agent' },
    global: { plugins: [createAppI18n('en-US')] }
  })
  try {
    await flushPromises()
    expect(ipc.mock.calls).toEqual([['list_unity_name_activity', { agentId: 'fixture-agent' }]])
    expect(wrapper.text()).toContain('Original object')
    expect(wrapper.text()).toContain('Scene 123, object -321.')
    await wrapper.get('[data-unity-approve]').trigger('click')
    await flushPromises()
    expect(ipc.mock.calls[1]).toEqual([
      'answer_unity_name_approval',
      { approvalId: 'fixture-approval', approved: true }
    ])
    expect(wrapper.text()).toContain('Renamed to “New object” in the editor.')
  } finally {
    wrapper.unmount()
  }
})
