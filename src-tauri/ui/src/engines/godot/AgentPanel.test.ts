import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, expect, it, vi } from 'vitest'
import { createAppI18n } from '../../i18n'
import AgentPanel from './AgentPanel.vue'
import { invoke } from '@tauri-apps/api/core'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))
beforeEach(() => {
  vi.resetAllMocks()
  vi.mocked(invoke).mockResolvedValue(undefined)
  localStorage.clear()
})

it('requires explicit model configuration and a connected fixed Godot target', async () => {
  const wrapper = mount(AgentPanel, {
    props: { sessionId: 'target-a', ready: true },
    global: { plugins: [createAppI18n('en-US')] }
  })
  expect(invoke).not.toHaveBeenCalled()
  expect((wrapper.get('input[name="agent-endpoint"]').element as HTMLInputElement).value).toBe('')
  expect((wrapper.get('input[name="agent-model"]').element as HTMLInputElement).value).toBe('')
  expect(wrapper.get('[data-agent-create]').attributes('disabled')).toBeDefined()
  vi.mocked(invoke).mockResolvedValueOnce({ agentId: 'agent-a' })
  await wrapper
    .get('input[name="agent-endpoint"]')
    .setValue('http://127.0.0.1:5555/v1/chat/completions')
  await wrapper.get('input[name="agent-model"]').setValue('local-test')
  await wrapper.get('input[name="agent-credential"]').setValue('fixture-secret')
  await wrapper.get('form.agent-form').trigger('submit')
  await flushPromises()
  expect(invoke).toHaveBeenCalledWith('create_godot_agent', {
    sessionId: 'target-a',
    endpoint: 'http://127.0.0.1:5555/v1/chat/completions',
    model: 'local-test',
    credential: 'fixture-secret'
  })
  expect((wrapper.get('input[name="agent-credential"]').element as HTMLInputElement).value).toBe('')
  expect(JSON.stringify(localStorage)).not.toContain('fixture-secret')
  wrapper.unmount()
})

it('offers rename only after opt-in and renders the exact scene, node, and approved name', async () => {
  const ipc = vi.mocked(invoke)
  let approved = false
  ipc.mockImplementation(async (command) => {
    if (command === 'create_godot_agent') return { agentId: 'agent-write' }
    if (command === 'answer_godot_name_approval') approved = true
    if (command === 'list_godot_name_activity') {
      return approved
        ? {
            approvals: [],
            receipts: [{ approvalId: 'request', status: 'applied', actualName: 'PlayerTwo' }]
          }
        : {
            approvals: [
              {
                approvalId: 'request',
                scenePath: 'res://main.tscn',
                nodePath: 'Actors/Player',
                oldName: 'Player',
                newName: 'PlayerTwo'
              }
            ],
            receipts: []
          }
    }
  })
  const wrapper = mount(AgentPanel, {
    props: { sessionId: 'fixed-target', ready: true },
    global: { plugins: [createAppI18n('en-US')] }
  })
  try {
    await wrapper.get('[data-godot-opt-into-rename]').setValue(true)
    await wrapper
      .get('input[name="agent-endpoint"]')
      .setValue('http://127.0.0.1:5555/v1/chat/completions')
    await wrapper.get('input[name="agent-model"]').setValue('fixture')
    await wrapper.get('form.agent-form').trigger('submit')
    await flushPromises()
    expect(ipc.mock.calls[0]).toEqual([
      'create_godot_agent',
      {
        sessionId: 'fixed-target',
        endpoint: 'http://127.0.0.1:5555/v1/chat/completions',
        model: 'fixture',
        credential: null,
        allowNodeRename: true
      }
    ])
    expect(wrapper.get('[data-godot-opt-into-rename]').attributes('disabled')).toBeDefined()
    expect(wrapper.text()).toContain('res://main.tscn · Actors/Player')
    expect(ipc.mock.calls.some(([name]) => name === 'answer_godot_name_approval')).toBe(false)
    await wrapper.get('[data-godot-approve]').trigger('click')
    await flushPromises()
    expect(ipc).toHaveBeenCalledWith('answer_godot_name_approval', {
      approvalId: 'request',
      approved: true
    })
    expect(wrapper.text()).toContain('Renamed to “PlayerTwo” in the editor.')
    await wrapper.setProps({ sessionId: 'new-target' })
    expect((wrapper.get('[data-godot-opt-into-rename]').element as HTMLInputElement).checked).toBe(
      false
    )
  } finally {
    wrapper.unmount()
  }
})

it('starts a saved model using its profile ID without an endpoint or credential in Vue', async () => {
  vi.mocked(invoke)
    .mockResolvedValueOnce({
      profiles: [{ id: 'saved-local', title: 'Saved fixture', authorization: 'keychain' }],
      bindings: { agent: 'saved-local' }
    })
    .mockResolvedValueOnce({ agentId: 'agent-saved' })
  const wrapper = mount(AgentPanel, {
    props: { sessionId: 'fixed-target', ready: true },
    global: { plugins: [createAppI18n('en-US')] }
  })
  expect(invoke).not.toHaveBeenCalled()
  await wrapper.get('select[name="agent-model-source"]').setValue('saved')
  await flushPromises()
  expect(invoke).toHaveBeenCalledExactlyOnceWith('get_model_settings')
  expect(wrapper.find('input[name="agent-credential"]').exists()).toBe(false)
  await wrapper.get('select[name="agent-model-profile"]').setValue('saved-local')
  await wrapper.get('form.agent-form').trigger('submit')
  await flushPromises()
  expect(invoke).toHaveBeenLastCalledWith('create_godot_agent_from_profile', {
    sessionId: 'fixed-target',
    profileId: 'saved-local'
  })
  wrapper.unmount()
})

it('discards a late poll response after cancellation and keeps draining visible', async () => {
  let resolvePoll: (value: unknown) => void = () => {}
  vi.mocked(invoke).mockImplementation((command) => {
    if (command === 'create_godot_agent') return Promise.resolve({ agentId: 'agent-a' })
    if (command === 'start_godot_agent') return Promise.resolve({ agentId: 'agent-a', runId: 1 })
    if (command === 'poll_godot_agent')
      return new Promise((resolve) => {
        resolvePoll = resolve
      })
    return Promise.resolve()
  })
  const wrapper = mount(AgentPanel, {
    props: { sessionId: 'target-a', ready: true },
    global: { plugins: [createAppI18n('en-US')] }
  })
  await wrapper
    .get('input[name="agent-endpoint"]')
    .setValue('http://127.0.0.1:5555/v1/chat/completions')
  await wrapper.get('input[name="agent-model"]').setValue('fixture')
  await wrapper.get('form.agent-form').trigger('submit')
  await flushPromises()
  await wrapper.get('textarea[name="agent-message"]').setValue('question')
  await wrapper.findAll('form.agent-form')[1].trigger('submit')
  await flushPromises()
  expect(invoke).toHaveBeenCalledWith('start_godot_agent', { agentId: 'agent-a', text: 'question' })
  await wrapper.get('[data-agent-cancel]').trigger('click')
  resolvePoll({
    agentId: 'agent-a',
    runId: 1,
    phase: 'draining',
    fragments: ['late secret result'],
    error: null
  })
  await flushPromises()
  expect(wrapper.text()).not.toContain('late secret result')
  expect(wrapper.text()).toContain('Draining')
  expect(wrapper.get('[data-agent-send]').attributes('disabled')).toBeDefined()
  wrapper.unmount()
})

it('drops old-session data when its target is released or replaced', async () => {
  let resolvePoll: (value: unknown) => void = () => {}
  vi.mocked(invoke).mockImplementation((command) => {
    if (command === 'create_godot_agent') return Promise.resolve({ agentId: 'agent-a' })
    if (command === 'start_godot_agent') return Promise.resolve({ agentId: 'agent-a', runId: 5 })
    if (command === 'poll_godot_agent')
      return new Promise((resolve) => {
        resolvePoll = resolve
      })
    return Promise.resolve()
  })
  const wrapper = mount(AgentPanel, {
    props: { sessionId: 'target-a', ready: true },
    global: { plugins: [createAppI18n('en-US')] }
  })
  await wrapper
    .get('input[name="agent-endpoint"]')
    .setValue('http://127.0.0.1:5555/v1/chat/completions')
  await wrapper.get('input[name="agent-model"]').setValue('fixture')
  await wrapper.get('form.agent-form').trigger('submit')
  await flushPromises()
  await wrapper.get('textarea[name="agent-message"]').setValue('question')
  await wrapper.findAll('form.agent-form')[1].trigger('submit')
  await flushPromises()
  await wrapper.setProps({ sessionId: 'target-b', ready: false })
  resolvePoll({
    agentId: 'agent-a',
    runId: 5,
    phase: 'completed',
    fragments: ['wrong target'],
    error: null
  })
  await flushPromises()
  expect(wrapper.text()).not.toContain('wrong target')
  expect(invoke).not.toHaveBeenCalledWith(
    'create_godot_agent',
    expect.objectContaining({ sessionId: 'target-b' })
  )
  wrapper.unmount()
})

it('never renders an unexpected error payload that could contain a credential', async () => {
  vi.mocked(invoke).mockRejectedValue('provider leaked fixture-secret')
  const wrapper = mount(AgentPanel, {
    props: { sessionId: 'target-a', ready: true },
    global: { plugins: [createAppI18n('en-US')] }
  })
  await wrapper
    .get('input[name="agent-endpoint"]')
    .setValue('http://127.0.0.1:5555/v1/chat/completions')
  await wrapper.get('input[name="agent-model"]').setValue('fixture')
  await wrapper.get('input[name="agent-credential"]').setValue('fixture-secret')
  await wrapper.get('form.agent-form').trigger('submit')
  await flushPromises()
  expect(wrapper.text()).not.toContain('fixture-secret')
  expect(wrapper.text()).toContain('operation failed')
  expect((wrapper.get('input[name="agent-credential"]').element as HTMLInputElement).value).toBe('')
  wrapper.unmount()
})

it('preserves the disconnect reason and prevents sending to a retired target', async () => {
  vi.mocked(invoke).mockImplementation((command) => {
    if (command === 'create_godot_agent') return Promise.resolve({ agentId: 'agent-a' })
    if (command === 'start_godot_agent') return Promise.resolve({ agentId: 'agent-a', runId: 1 })
    if (command === 'poll_godot_agent') {
      return Promise.resolve({
        agentId: 'agent-a',
        runId: 1,
        phase: 'failed',
        fragments: [],
        error: 'target_unavailable'
      })
    }
    return Promise.resolve()
  })
  const wrapper = mount(AgentPanel, {
    props: { sessionId: 'target-a', ready: true },
    global: { plugins: [createAppI18n('en-US')] }
  })
  try {
    await wrapper
      .get('input[name="agent-endpoint"]')
      .setValue('http://127.0.0.1:5555/v1/chat/completions')
    await wrapper.get('input[name="agent-model"]').setValue('fixture')
    await wrapper.get('form.agent-form').trigger('submit')
    await flushPromises()
    const message = wrapper.get('textarea[name="agent-message"]')
    await message.setValue('question')
    await wrapper.findAll('form.agent-form')[1].trigger('submit')
    await flushPromises()
    expect(wrapper.text()).toContain('The selected Godot connection is no longer available.')
    expect(message.attributes('disabled')).toBeDefined()
    expect(wrapper.get('[data-agent-close]').attributes('disabled')).toBeUndefined()
  } finally {
    wrapper.unmount()
  }
})

it('ignores text and phase carrying a different run identity', async () => {
  let resolvePoll: (value: unknown) => void = () => {}
  vi.mocked(invoke).mockImplementation((command) => {
    if (command === 'create_godot_agent') return Promise.resolve({ agentId: 'agent-a' })
    if (command === 'start_godot_agent') return Promise.resolve({ agentId: 'agent-a', runId: 2 })
    if (command === 'poll_godot_agent')
      return new Promise((resolve) => {
        resolvePoll = resolve
      })
    return Promise.resolve()
  })
  const wrapper = mount(AgentPanel, {
    props: { sessionId: 'target-a', ready: true },
    global: { plugins: [createAppI18n('en-US')] }
  })
  await wrapper
    .get('input[name="agent-endpoint"]')
    .setValue('http://127.0.0.1:5555/v1/chat/completions')
  await wrapper.get('input[name="agent-model"]').setValue('fixture')
  await wrapper.get('form.agent-form').trigger('submit')
  await flushPromises()
  await wrapper.get('textarea[name="agent-message"]').setValue('question')
  await wrapper.findAll('form.agent-form')[1].trigger('submit')
  await flushPromises()
  resolvePoll({
    agentId: 'agent-a',
    runId: 1,
    phase: 'completed',
    fragments: ['stale output'],
    error: null
  })
  await flushPromises()
  expect(wrapper.text()).not.toContain('stale output')
  expect(wrapper.text()).toContain('Responding')
  expect(wrapper.get('[data-agent-send]').attributes('disabled')).toBeDefined()
  wrapper.unmount()
})

it('keeps cancellation visible when an earlier running poll arrives late', async () => {
  let resolvePoll: (value: unknown) => void = () => {}
  vi.mocked(invoke).mockImplementation((command) => {
    if (command === 'create_godot_agent') return Promise.resolve({ agentId: 'agent-a' })
    if (command === 'start_godot_agent') return Promise.resolve({ agentId: 'agent-a', runId: 1 })
    if (command === 'poll_godot_agent')
      return new Promise((resolve) => {
        resolvePoll = resolve
      })
    return Promise.resolve()
  })
  const wrapper = mount(AgentPanel, {
    props: { sessionId: 'target-a', ready: true },
    global: { plugins: [createAppI18n('en-US')] }
  })
  await wrapper
    .get('input[name="agent-endpoint"]')
    .setValue('http://127.0.0.1:5555/v1/chat/completions')
  await wrapper.get('input[name="agent-model"]').setValue('fixture')
  await wrapper.get('form.agent-form').trigger('submit')
  await flushPromises()
  await wrapper.get('textarea[name="agent-message"]').setValue('question')
  await wrapper.findAll('form.agent-form')[1].trigger('submit')
  await flushPromises()
  await wrapper.get('[data-agent-cancel]').trigger('click')
  resolvePoll({ agentId: 'agent-a', runId: 1, phase: 'running', fragments: ['late'], error: null })
  await flushPromises()
  expect(wrapper.text()).toContain('Cancelling')
  expect(wrapper.text()).not.toContain('late')
  wrapper.unmount()
})
