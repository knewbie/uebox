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
