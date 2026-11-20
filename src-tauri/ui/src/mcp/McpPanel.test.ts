import { invoke } from '@tauri-apps/api/core'
import { flushPromises, mount } from '@vue/test-utils'
import { expect, it, vi } from 'vitest'
import { createAppI18n } from '../i18n'
import McpPanel from './McpPanel.vue'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))

it('inspects only after submit and treats advertised names as text', async () => {
  const ipc = vi.mocked(invoke)
  ipc.mockReset().mockResolvedValueOnce({
    protocolVersion: '2025-11-25',
    totalTools: 201,
    toolNames: ['<img src="/">', 'fixture.list'],
    sessionCleanupPending: true
  })
  const wrapper = mount(McpPanel, { global: { plugins: [createAppI18n('zh-CN')] } })
  try {
    const endpoint = 'http://127.0.0.1:34567/mcp'
    await wrapper.get('#mcp-endpoint').setValue(endpoint)
    expect(ipc).not.toHaveBeenCalled()
    await wrapper.get('form').trigger('submit')
    await flushPromises()
    expect(ipc.mock.calls).toEqual([['inspect_mcp_endpoint', { endpoint }]])
    expect(wrapper.get('[role="status"]').text()).toContain('共 201 个工具')
    expect(wrapper.findAll('code').map((node) => node.text())).toEqual([
      '<img src="/">',
      'fixture.list'
    ])
    expect(wrapper.find('img').exists()).toBe(false)
    expect(wrapper.text()).toContain('服务器未确认已结束本次连接。')
  } finally {
    wrapper.unmount()
  }
})

it('registers without making a network request and inspects a saved endpoint by ID', async () => {
  const ipc = vi.mocked(invoke)
  const saved = {
    id: 'endpoint-fixture',
    title: 'Local editor',
    url: 'http://127.0.0.1:34567/mcp',
    authenticated: true
  }
  ipc.mockReset().mockImplementation(async (command) => {
    if (command === 'list_mcp_endpoints') return []
    if (command === 'save_mcp_endpoint') {
      return { endpoints: [saved], credentialCleanupPending: false }
    }
    if (command === 'inspect_saved_mcp_endpoint') {
      return {
        protocolVersion: '2025-11-25',
        totalTools: 1,
        toolNames: ['editor.status'],
        sessionCleanupPending: false
      }
    }
    if (command === 'remove_mcp_endpoint') {
      return { endpoints: [], credentialCleanupPending: false }
    }
  })
  const wrapper = mount(McpPanel, { global: { plugins: [createAppI18n('en-US')] } })
  try {
    await wrapper.get('[data-mcp-saved]').trigger('click')
    await flushPromises()
    expect(ipc.mock.calls).toEqual([['list_mcp_endpoints']])
    await wrapper.get('#mcp-save-title').setValue(saved.title)
    await wrapper.get('#mcp-save-url').setValue(saved.url)
    await wrapper.get('#mcp-save-bearer').setValue('fixture-secret')
    await wrapper.get('form.saved-form').trigger('submit')
    await flushPromises()
    expect(ipc).toHaveBeenLastCalledWith('save_mcp_endpoint', {
      title: saved.title,
      endpoint: saved.url,
      bearer: 'fixture-secret'
    })
    expect((wrapper.get('#mcp-save-bearer').element as HTMLInputElement).value).toBe('')
    expect(wrapper.text()).not.toContain('fixture-secret')
    const entry = wrapper.get('.saved-item')
    await entry.get('button').trigger('click')
    await flushPromises()
    expect(ipc).toHaveBeenLastCalledWith('inspect_saved_mcp_endpoint', { endpointId: saved.id })
    expect(wrapper.text()).toContain('editor.status')
    await entry.findAll('button')[1].trigger('click')
    expect(wrapper.text()).toContain('Remove “Local editor”')
    expect(ipc.mock.calls.some(([command]) => command === 'remove_mcp_endpoint')).toBe(false)
    await wrapper.findAll('.saved-actions')[1].get('button').trigger('click')
    await flushPromises()
    expect(ipc).toHaveBeenLastCalledWith('remove_mcp_endpoint', { endpointId: saved.id })
    expect(wrapper.find('.saved-item').exists()).toBe(false)
  } finally {
    wrapper.unmount()
  }
})
