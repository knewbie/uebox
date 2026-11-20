import { invoke } from '@tauri-apps/api/core'
import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, expect, it, vi } from 'vitest'
import { createAppI18n } from '../../i18n'
import ActorApprovalPanel from './ActorApprovalPanel.vue'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))
afterEach(() => vi.resetAllMocks())

it('shows the requested change and submits only its exact approval ID', async () => {
  const ipc = vi.mocked(invoke)
  ipc.mockResolvedValueOnce({
    approvals: [
      { approvalId: '7', actorPath: '/Game/Test.Actor', oldLabel: 'Before', newLabel: 'After' }
    ],
    receipts: []
  })
  ipc.mockResolvedValueOnce(undefined)
  ipc.mockResolvedValueOnce({
    approvals: [],
    receipts: [{ approvalId: '7', status: 'unknown', actualLabel: null }]
  })
  const wrapper = mount(ActorApprovalPanel, {
    props: { enabled: true, agentId: 'agent' },
    global: { plugins: [createAppI18n('en-US')] }
  })
  try {
    await flushPromises()
    expect(wrapper.text()).toContain('/Game/Test.Actor')
    expect(wrapper.text()).toContain('Current label Before')
    expect(wrapper.text()).toContain('Proposed label After')
    expect(ipc).toHaveBeenCalledExactlyOnceWith('list_unreal_actor_approvals', { agentId: 'agent' })
    await wrapper.get('[data-actor-approve]').trigger('click')
    await flushPromises()
    expect(ipc).toHaveBeenNthCalledWith(2, 'answer_unreal_actor_approval', {
      approvalId: '7',
      allowed: true
    })
    expect(wrapper.find('[data-actor-approve]').exists()).toBe(false)
    expect(wrapper.text()).toContain('The result is unknown')
  } finally {
    wrapper.unmount()
  }
})
