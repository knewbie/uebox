import { mount, flushPromises } from '@vue/test-utils'
import { defineComponent, h } from 'vue'
import { useI18n } from 'vue-i18n'
import zhCN from '@renderer/i18n/locales/zh-CN'
import enUS from '@renderer/i18n/locales/en-US'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import App from './App.vue'
import { createAppI18n } from './i18n'
import { getHostStatus } from './host'
import { inspectUnity } from './engines/unity/commands'
import {
  startUnrealListener,
  stopUnrealListener,
  listUnrealInstances,
  queryUnrealProject
} from './engines/unreal/commands'
import { invoke } from '@tauri-apps/api/core'

vi.mock('./host', () => ({ getHostStatus: vi.fn() }))
vi.mock('./engines/unity/commands', () => ({ inspectUnity: vi.fn() }))
vi.mock('./engines/unreal/commands', () => ({
  startUnrealListener: vi.fn(),
  stopUnrealListener: vi.fn(),
  listUnrealInstances: vi.fn(),
  queryUnrealProject: vi.fn()
}))
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))

const actualHostStatus = {
  backend: 'rust',
  processId: 4321,
  os: 'macos',
  architecture: 'aarch64',
  version: '1.0.12',
  migration: 'in_progress',
  engineModules: { unity: true, unreal: true, godot: true },
  capabilities: [
    { id: 'engines', available: false },
    { id: 'agent', available: false },
    { id: 'libraries', available: false },
    { id: 'settings', available: false }
  ]
} as const

const unitySnapshot = {
  project: {
    projectRoot: '/tmp/Unity fixture',
    projectName: 'Real fixture project',
    unityVersion: '2022.3.fixture',
    assetsPath: '/tmp/Unity fixture/Assets',
    platform: 'Fixture platform',
    renderPipeline: 'Fixture pipeline'
  },
  scene: {
    name: 'Fixture scene',
    path: 'Assets/Fixture.unity',
    buildIndex: -1,
    isDirty: true,
    isLoaded: true,
    rootCount: 3
  }
} as const

const unrealInstance = {
  connection_id: 'ue-test-first',
  project_name: 'Editor A',
  project_path: '/fixture/ProjectA.uproject',
  interactive: true
} as const

beforeEach(() => {
  localStorage.clear()
  vi.resetAllMocks()
  vi.mocked(invoke).mockResolvedValue(undefined)
})

describe('Tauri migration preview', () => {
  it('opens a Unity assistant target only after an explicit takeover', async () => {
    vi.mocked(getHostStatus).mockResolvedValue(actualHostStatus)
    vi.mocked(invoke).mockResolvedValueOnce({ sessionId: 'unity-target-1' })
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()

    await wrapper.get('input[name="unity-project"]').setValue('/tmp/Unity fixture')
    await wrapper.get('input[name="unity-port"]').setValue('55234')
    expect(wrapper.get('[data-unity-attach-agent]').attributes('disabled')).toBeDefined()
    expect(invoke).not.toHaveBeenCalled()
    await wrapper.get('button[role="checkbox"]').trigger('click')
    await wrapper.get('[data-unity-attach-agent]').trigger('click')
    await flushPromises()
    expect(invoke).toHaveBeenCalledExactlyOnceWith('attach_unity_agent_target', {
      projectRoot: '/tmp/Unity fixture',
      port: 55234,
      allowClientTakeover: true
    })
    expect(wrapper.text()).toContain('unity-target-1')
    expect(wrapper.get('[data-unity-query]').attributes('disabled')).toBeDefined()
    wrapper.unmount()
    expect(invoke).toHaveBeenLastCalledWith('release_unity_agent_target', {
      sessionId: 'unity-target-1'
    })
  })

  it('passes an explicitly entered Unreal root and selected connection to the assistant', async () => {
    vi.mocked(getHostStatus).mockResolvedValue(actualHostStatus)
    vi.mocked(startUnrealListener).mockResolvedValue({ address: '127.0.0.1', port: 17860 })
    vi.mocked(listUnrealInstances).mockResolvedValue([unrealInstance])
    vi.mocked(invoke).mockResolvedValueOnce({ agentId: 'unreal-agent-1' })
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()
    await wrapper.get('[data-unreal-start]').trigger('click')
    await flushPromises()
    await wrapper.get('[data-unreal-select="ue-test-first"]').trigger('click')

    expect(wrapper.find('[data-agent-create]').exists()).toBe(false)
    await wrapper.get('input[name="unreal-agent-project"]').setValue('/fixture')
    await wrapper
      .get('input[name="agent-endpoint"]')
      .setValue('http://127.0.0.1:5555/v1/chat/completions')
    await wrapper.get('input[name="agent-model"]').setValue('fixture-model')
    await wrapper.get('#unreal-heading ~ .agent form').trigger('submit')
    await flushPromises()
    expect(invoke).toHaveBeenCalledExactlyOnceWith('create_unreal_agent', {
      connectionId: 'ue-test-first',
      projectRoot: '/fixture',
      endpoint: 'http://127.0.0.1:5555/v1/chat/completions',
      model: 'fixture-model',
      credential: null
    })
    wrapper.unmount()
  })

  it('lets Unity connect again after the previous target has already disconnected', async () => {
    vi.mocked(getHostStatus).mockResolvedValue(actualHostStatus)
    vi.mocked(invoke)
      .mockResolvedValueOnce({ sessionId: 'unity-target-1' })
      .mockRejectedValueOnce('target_unavailable')
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()

    await wrapper.get('input[name="unity-project"]').setValue('/tmp/Unity fixture')
    await wrapper.get('input[name="unity-port"]').setValue('55234')
    await wrapper.get('button[role="checkbox"]').trigger('click')
    await wrapper.get('[data-unity-attach-agent]').trigger('click')
    await flushPromises()
    await wrapper.get('[data-unity-release-agent]').trigger('click')
    await flushPromises()

    expect(wrapper.text()).not.toContain('unity-target-1')
    expect(wrapper.get('input[name="unity-port"]').attributes('disabled')).toBeUndefined()
    expect(wrapper.get('[data-unity-attach-agent]').attributes('disabled')).toBeDefined()
    wrapper.unmount()
  })

  it('keeps both shared preview namespaces aligned', () => {
    const leafKeys = (value: Record<string, unknown>, prefix = ''): string[] =>
      Object.entries(value).flatMap(([key, item]) => {
        const path = prefix ? `${prefix}.${key}` : key
        return item && typeof item === 'object'
          ? leafKeys(item as Record<string, unknown>, path)
          : [path]
      })

    expect(leafKeys(zhCN.tauriPreview).sort()).toEqual(leafKeys(enUS.tauriPreview).sort())
  })

  it('reads preview translations from the shared bilingual namespace', () => {
    const probe = defineComponent({
      setup() {
        const { t } = useI18n()
        return () =>
          h(
            'div',
            `${t('tauriPreview.unity.title')} | ${t('tauriPreview.unreal.title')} | ${t('menu.projectLib')}`
          )
      }
    })
    const chinese = mount(probe, { global: { plugins: [createAppI18n('zh-CN')] } })
    const english = mount(probe, { global: { plugins: [createAppI18n('en-US')] } })

    expect(chinese.text()).toBe('Unity 只读查询 | Unreal Engine 只读查询 | 项目库')
    expect(english.text()).toBe(
      'Unity read-only query | Unreal Engine read-only query | Project Library'
    )
    chinese.unmount()
    english.unmount()
  })

  it('requests host status through the dedicated Tauri command', async () => {
    const host = await vi.importActual<typeof import('./host')>('./host')
    vi.mocked(invoke).mockResolvedValue(actualHostStatus)

    expect(await host.getHostStatus()).toEqual(actualHostStatus)
    expect(invoke).toHaveBeenCalledWith('host_status')
  })

  it('links to project registration when the host enables it with every engine disabled', async () => {
    const capabilities = [{ id: 'projectRegistration', available: true }] as const
    vi.mocked(getHostStatus).mockResolvedValue({
      ...actualHostStatus,
      capabilities,
      engineModules: { unity: false, unreal: false, godot: false }
    })
    vi.mocked(invoke).mockResolvedValue([])
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()

    expect(wrapper.get('nav a[href="#project-registration"]').text()).toBe('Project Library')
    expect(wrapper.get('#project-registration').text()).toContain('Project registration')
    expect(invoke).toHaveBeenCalledExactlyOnceWith('list_registered_projects')
    wrapper.unmount()
  })

  it.each([
    { capabilities: [] },
    { capabilities: [{ id: 'projectRegistration', available: false }] as const }
  ])(
    'keeps project registration unavailable without an enabled capability (%j)',
    async ({ capabilities }) => {
      vi.mocked(getHostStatus).mockResolvedValue({ ...actualHostStatus, capabilities })
      const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
      await flushPromises()

      expect(wrapper.find('nav a[href="#project-registration"]').exists()).toBe(false)
      expect(wrapper.find('#project-registration').exists()).toBe(false)
      expect(invoke).not.toHaveBeenCalledWith('list_registered_projects')
      wrapper.unmount()
    }
  )

  it('does not start the UE listener before the user clicks', async () => {
    vi.mocked(getHostStatus).mockResolvedValue(actualHostStatus)
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()

    expect((wrapper.get('input[name="unreal-port"]').element as HTMLInputElement).value).toBe(
      '17860'
    )
    expect(vi.mocked(startUnrealListener)).not.toHaveBeenCalled()
    expect(vi.mocked(listUnrealInstances)).not.toHaveBeenCalled()
    expect(vi.mocked(queryUnrealProject)).not.toHaveBeenCalled()
    wrapper.unmount()
  })

  it('marks Unreal unavailable and exposes no Unreal actions in a Unity-only host', async () => {
    vi.mocked(getHostStatus).mockResolvedValue({
      ...actualHostStatus,
      engineModules: { unity: true, unreal: false, godot: false }
    })
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()
    expect(wrapper.get('[data-engine-unavailable="unreal"]').text()).toContain('Not yet available')
    expect(wrapper.text()).not.toContain('Unreal and Unity have limited read-only queries')
    expect(wrapper.find('[data-unreal-start]').exists()).toBe(false)
    expect(wrapper.find('[data-unity-query]').exists()).toBe(true)
    expect(startUnrealListener).not.toHaveBeenCalled()
    await wrapper.get('button[lang="zh-CN"]').trigger('click')
    expect(wrapper.get('[data-engine-unavailable="unreal"]').text()).toContain('尚不可用')
    expect(wrapper.text()).not.toContain('Unreal 和 Unity 仅支持有限的只读查询')
    wrapper.unmount()
  })

  it('marks Unity unavailable and exposes no Unity actions in an Unreal-only host', async () => {
    vi.mocked(getHostStatus).mockResolvedValue({
      ...actualHostStatus,
      engineModules: { unity: false, unreal: true, godot: false }
    })
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()
    expect(wrapper.get('[data-engine-unavailable="unity"]').text()).toContain('Not yet available')
    expect(wrapper.text()).not.toContain('Unreal and Unity have limited read-only queries')
    expect(wrapper.find('[data-unity-query]').exists()).toBe(false)
    expect(wrapper.find('[data-unreal-start]').exists()).toBe(true)
    expect(inspectUnity).not.toHaveBeenCalled()
    wrapper.unmount()
  })

  it('marks both engines unavailable when no engine module is compiled', async () => {
    vi.mocked(getHostStatus).mockResolvedValue({
      ...actualHostStatus,
      engineModules: { unity: false, unreal: false, godot: false }
    })
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()
    expect(wrapper.get('[data-engine-unavailable="unity"]').text()).toContain('Not yet available')
    expect(wrapper.get('[data-engine-unavailable="unreal"]').text()).toContain('Not yet available')
    expect(wrapper.get('[data-engine-unavailable="godot"]').text()).toContain('Not yet available')
    expect(wrapper.find('[data-unity-query]').exists()).toBe(false)
    expect(wrapper.find('[data-unreal-start]').exists()).toBe(false)
    wrapper.unmount()
  })

  it('starts the explicitly selected Godot target without a separate confirmation', async () => {
    vi.mocked(getHostStatus).mockResolvedValue(actualHostStatus)
    vi.mocked(invoke).mockResolvedValue({ sessionId: 'host-fixture-id', state: 'starting' })
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()
    const panel = wrapper.get('[aria-labelledby="godot-heading"]')
    expect(panel.text()).toContain('plugin must already be enabled')
    expect(panel.text()).toContain('does not close the editor')
    expect((panel.get('input[name="godot-executable"]').element as HTMLInputElement).value).toBe('')
    expect((panel.get('input[name="godot-project"]').element as HTMLInputElement).value).toBe('')
    expect(panel.get('[data-godot-launch]').attributes('disabled')).toBeDefined()
    await panel.get('input[name="godot-executable"]').setValue('/fixture/Godot')
    await panel.get('input[name="godot-project"]').setValue('/fixture/project')
    expect(panel.find('button[role="checkbox"]').exists()).toBe(false)
    expect(panel.get('[data-godot-launch]').attributes('disabled')).toBeUndefined()
    expect(invoke).not.toHaveBeenCalled()
    await panel.get('form').trigger('submit')
    await flushPromises()
    expect(invoke).toHaveBeenCalledExactlyOnceWith('launch_godot', {
      executable: '/fixture/Godot',
      project: '/fixture/project'
    })
    expect(panel.text()).toContain('host-fixture-id')
    expect(panel.find('[data-godot-connect]').exists()).toBe(true)
    wrapper.unmount()
  })

  it('requires absolute Godot executable and project paths before launch', async () => {
    vi.mocked(getHostStatus).mockResolvedValue(actualHostStatus)
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()
    const panel = wrapper.get('[aria-labelledby="godot-heading"]')
    await panel.get('input[name="godot-executable"]').setValue('relative/Godot')
    await panel.get('input[name="godot-project"]').setValue('/fixture/project')
    expect(panel.get('[data-godot-launch]').attributes('disabled')).toBeDefined()
    await panel.get('input[name="godot-executable"]').setValue('/fixture/Godot')
    expect(panel.get('[data-godot-launch]').attributes('disabled')).toBeUndefined()
    await panel.get('input[name="godot-project"]').setValue('relative/project')
    expect(panel.get('[data-godot-launch]').attributes('disabled')).toBeDefined()
    await panel.get('form').trigger('submit')
    expect(invoke).not.toHaveBeenCalled()
    wrapper.unmount()
  })

  it('shows a Godot launch error without retrying automatically', async () => {
    vi.mocked(getHostStatus).mockResolvedValue(actualHostStatus)
    vi.mocked(invoke).mockRejectedValue('Godot session error: InvalidExecutable')
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()
    const panel = wrapper.get('[aria-labelledby="godot-heading"]')
    await panel.get('input[name="godot-executable"]').setValue('/fixture/Godot')
    await panel.get('input[name="godot-project"]').setValue('/fixture/project')
    await panel.get('form').trigger('submit')
    await flushPromises()
    expect(panel.text()).toContain('Godot session error: InvalidExecutable')
    expect(panel.get('[data-godot-launch]').attributes('disabled')).toBeUndefined()
    expect(invoke).toHaveBeenCalledTimes(1)
    wrapper.unmount()
  })

  it('does not start a second Godot editor while the first launch is pending', async () => {
    vi.mocked(getHostStatus).mockResolvedValue(actualHostStatus)
    let finish: (value: { sessionId: string; state: 'starting' }) => void = () => {}
    vi.mocked(invoke).mockReturnValue(
      new Promise((resolve) => {
        finish = resolve
      })
    )
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()
    const panel = wrapper.get('[aria-labelledby="godot-heading"]')
    await panel.get('input[name="godot-executable"]').setValue('/fixture/Godot')
    await panel.get('input[name="godot-project"]').setValue('/fixture/project')
    await panel.get('form').trigger('submit')
    await panel.get('form').trigger('submit')
    expect(invoke).toHaveBeenCalledTimes(1)
    expect(panel.get('[data-godot-launch]').attributes('disabled')).toBeDefined()
    finish({ sessionId: 'host-fixture-id', state: 'starting' })
    await flushPromises()
    wrapper.unmount()
  })

  it('routes explicit Godot operations by the host session ID without a token', async () => {
    vi.mocked(getHostStatus).mockResolvedValue(actualHostStatus)
    vi.mocked(invoke)
      .mockResolvedValueOnce({ sessionId: 'host-fixture-id', state: 'starting' })
      .mockResolvedValueOnce({ name: 'Fixture project', godotVersion: '4.fixture' })
      .mockResolvedValueOnce({ name: 'Updated project', godotVersion: '4.fixture' })
      .mockResolvedValueOnce({
        scenePath: 'res://main.tscn',
        nodes: [{ path: '.', name: 'Fixture root', class: 'Node' }],
        truncated: false
      })
      .mockResolvedValueOnce({ state: 'ready', exit: null })
      .mockResolvedValueOnce(undefined)
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()
    const panel = wrapper.get('[aria-labelledby="godot-heading"]')
    await panel.get('input[name="godot-executable"]').setValue('/fixture/Godot')
    await panel.get('input[name="godot-project"]').setValue('/fixture/project')
    await panel.get('form').trigger('submit')
    await flushPromises()
    expect(invoke).toHaveBeenCalledTimes(1)
    expect(panel.get('[data-godot-scene]').attributes('disabled')).toBeDefined()
    for (const selector of ['connect', 'project', 'scene', 'status', 'release']) {
      await panel.get(`[data-godot-${selector}]`).trigger('click')
      await flushPromises()
      if (selector === 'scene') {
        expect(panel.text()).toContain('Updated project')
        expect(panel.text()).toContain('Fixture root')
      }
    }
    expect(vi.mocked(invoke).mock.calls).toEqual([
      ['launch_godot', { executable: '/fixture/Godot', project: '/fixture/project' }],
      ['connect_godot', { sessionId: 'host-fixture-id' }],
      ['query_godot_project', { sessionId: 'host-fixture-id' }],
      ['query_godot_scene', { sessionId: 'host-fixture-id' }],
      ['status_godot_session', { sessionId: 'host-fixture-id' }],
      ['release_godot_session', { sessionId: 'host-fixture-id' }]
    ])
    expect(panel.text()).not.toContain('host-fixture-id')
    wrapper.unmount()
  })

  it('does not retain old Godot data or retry when a later query fails', async () => {
    vi.mocked(getHostStatus).mockResolvedValue(actualHostStatus)
    vi.mocked(invoke)
      .mockResolvedValueOnce({ sessionId: 'host-fixture-id', state: 'starting' })
      .mockResolvedValueOnce({ name: 'Old project', godotVersion: '4.fixture' })
      .mockResolvedValueOnce({ scenePath: 'res://old.tscn', nodes: [], truncated: false })
      .mockRejectedValueOnce('Godot session error: Timeout')
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()
    const panel = wrapper.get('[aria-labelledby="godot-heading"]')
    await panel.get('input[name="godot-executable"]').setValue('/fixture/Godot')
    await panel.get('input[name="godot-project"]').setValue('/fixture/project')
    await panel.get('form').trigger('submit')
    await flushPromises()
    await panel.get('[data-godot-connect]').trigger('click')
    await flushPromises()
    await panel.get('[data-godot-scene]').trigger('click')
    await flushPromises()
    await panel.get('[data-godot-project]').trigger('click')
    await flushPromises()
    expect(panel.text()).toContain('Godot session error: Timeout')
    expect(panel.text()).not.toContain('Old project')
    expect(panel.text()).not.toContain('res://old.tscn')
    expect(invoke).toHaveBeenCalledTimes(4)
    expect(panel.get('[data-godot-project]').attributes('disabled')).toBeDefined()
    wrapper.unmount()
  })

  it('does not allow engine actions when host module availability is unknown', async () => {
    vi.mocked(getHostStatus).mockRejectedValue('host unavailable')
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()
    expect(wrapper.get('[data-unreal-start]').attributes('disabled')).toBeDefined()
    await wrapper.get('[data-unreal-start]').trigger('click')
    expect(startUnrealListener).not.toHaveBeenCalled()
    await wrapper.get('input[name="godot-executable"]').setValue('/fixture/Godot')
    await wrapper.get('input[name="godot-project"]').setValue('/fixture/project')
    expect(wrapper.get('[data-godot-launch]').attributes('disabled')).toBeDefined()
    wrapper.unmount()
  })

  it('enables Unreal only after the host confirms that its module is present', async () => {
    let finish: (status: typeof actualHostStatus) => void = () => {}
    vi.mocked(getHostStatus).mockReturnValue(
      new Promise((resolve) => {
        finish = resolve
      })
    )
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()
    expect(wrapper.get('[data-unreal-start]').attributes('disabled')).toBeDefined()
    await wrapper.get('[data-unreal-start]').trigger('click')
    expect(startUnrealListener).not.toHaveBeenCalled()
    finish(actualHostStatus)
    await flushPromises()
    expect(wrapper.get('[data-unreal-start]').attributes('disabled')).toBeUndefined()
    wrapper.unmount()
  })

  it('starts a selected port, requires a list selection and displays the engine result', async () => {
    vi.mocked(getHostStatus).mockResolvedValue(actualHostStatus)
    vi.mocked(startUnrealListener).mockResolvedValue({ address: '127.0.0.1', port: 55290 })
    vi.mocked(listUnrealInstances).mockResolvedValue([
      unrealInstance,
      { ...unrealInstance, connection_id: 'ue-test-second', project_name: 'Editor B' }
    ])
    vi.mocked(queryUnrealProject).mockResolvedValue({ projectName: 'Actual engine response' })
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()

    await wrapper.get('input[name="unreal-port"]').setValue('55290')
    await wrapper.get('[data-unreal-start]').trigger('click')
    await flushPromises()
    expect(startUnrealListener).toHaveBeenCalledExactlyOnceWith(55290)
    expect(wrapper.text()).toContain('Editor A')
    expect(wrapper.text()).toContain('Editor B')
    expect(wrapper.get('[data-unreal-query]').attributes('disabled')).toBeDefined()
    await wrapper.get('[data-unreal-select="ue-test-second"]').trigger('click')
    await wrapper.get('[data-unreal-query]').trigger('click')
    await flushPromises()
    expect(queryUnrealProject).toHaveBeenCalledExactlyOnceWith('ue-test-second')
    expect(wrapper.text()).toContain('Actual engine response')
    wrapper.unmount()
  })

  it('invalidates an old selection after the project changes', async () => {
    vi.mocked(getHostStatus).mockResolvedValue(actualHostStatus)
    vi.mocked(startUnrealListener).mockResolvedValue({ address: '127.0.0.1', port: 55290 })
    vi.mocked(listUnrealInstances)
      .mockResolvedValueOnce([unrealInstance])
      .mockResolvedValueOnce([
        { ...unrealInstance, connection_id: 'ue-test-second', project_name: 'Editor B' }
      ])
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()
    await wrapper.get('input[name="unreal-port"]').setValue('55290')
    await wrapper.get('[data-unreal-start]').trigger('click')
    await flushPromises()
    await wrapper.get('[data-unreal-select="ue-test-first"]').trigger('click')
    await wrapper.get('[data-unreal-refresh]').trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('Editor B')
    expect(wrapper.get('[data-unreal-query]').attributes('disabled')).toBeDefined()
    expect(queryUnrealProject).not.toHaveBeenCalled()
    wrapper.unmount()
  })

  it('shows a real bind error and does not claim a listener started', async () => {
    vi.mocked(getHostStatus).mockResolvedValue(actualHostStatus)
    vi.mocked(startUnrealListener).mockRejectedValue('Address already in use (os error 48)')
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()
    await wrapper.get('[data-unreal-start]').trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('Address already in use (os error 48)')
    expect(listUnrealInstances).not.toHaveBeenCalled()
    expect(wrapper.get('[data-unreal-stop]').attributes('disabled')).toBeDefined()
    wrapper.unmount()
  })

  it('uses only the dedicated UE listener and project commands', async () => {
    const host = await vi.importActual<typeof import('./engines/unreal/commands')>(
      './engines/unreal/commands'
    )
    vi.mocked(invoke)
      .mockResolvedValueOnce({ address: '127.0.0.1', port: 55290 })
      .mockResolvedValueOnce([unrealInstance])
      .mockResolvedValueOnce({ projectName: 'Engine data' })
      .mockResolvedValueOnce(undefined)
    await host.startUnrealListener(55290)
    await host.listUnrealInstances()
    await host.queryUnrealProject('ue-test-first')
    await host.stopUnrealListener()
    expect(vi.mocked(invoke).mock.calls).toEqual([
      ['start_unreal_listener', { port: 55290 }],
      ['list_unreal_instances'],
      ['query_unreal_project', { connectionId: 'ue-test-first' }],
      ['stop_unreal_listener']
    ])
  })

  it('shows a stale-selection error and requires a new selection', async () => {
    vi.mocked(getHostStatus).mockResolvedValue(actualHostStatus)
    vi.mocked(startUnrealListener).mockResolvedValue({ address: '127.0.0.1', port: 55290 })
    vi.mocked(listUnrealInstances).mockResolvedValue([unrealInstance])
    vi.mocked(queryUnrealProject).mockRejectedValue('instance is not connected and identified')
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()
    await wrapper.get('input[name="unreal-port"]').setValue('55290')
    await wrapper.get('[data-unreal-start]').trigger('click')
    await flushPromises()
    await wrapper.get('[data-unreal-select="ue-test-first"]').trigger('click')
    await wrapper.get('[data-unreal-query]').trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('instance is not connected and identified')
    expect(wrapper.get('[data-unreal-query]').attributes('disabled')).toBeDefined()
    expect(queryUnrealProject).toHaveBeenCalledTimes(1)
    wrapper.unmount()
  })

  it('shows a timeout without claiming the instance selection became stale or retrying', async () => {
    vi.mocked(getHostStatus).mockResolvedValue(actualHostStatus)
    vi.mocked(startUnrealListener).mockResolvedValue({ address: '127.0.0.1', port: 55290 })
    vi.mocked(listUnrealInstances).mockResolvedValue([unrealInstance])
    vi.mocked(queryUnrealProject).mockRejectedValue('request timed out; its outcome is unknown')
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()
    await wrapper.get('input[name="unreal-port"]').setValue('55290')
    await wrapper.get('[data-unreal-start]').trigger('click')
    await flushPromises()
    await wrapper.get('[data-unreal-select="ue-test-first"]').trigger('click')
    await wrapper.get('[data-unreal-query]').trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('request timed out; its outcome is unknown')
    expect(wrapper.text()).not.toContain('The previous selection is no longer valid')
    expect(wrapper.get('[data-unreal-query]').attributes('disabled')).toBeDefined()
    expect(queryUnrealProject).toHaveBeenCalledTimes(1)
    wrapper.unmount()
  })

  it('stops while a query is pending and ignores the old result', async () => {
    vi.mocked(getHostStatus).mockResolvedValue(actualHostStatus)
    vi.mocked(startUnrealListener).mockResolvedValue({ address: '127.0.0.1', port: 55290 })
    vi.mocked(listUnrealInstances).mockResolvedValue([unrealInstance])
    vi.mocked(stopUnrealListener).mockResolvedValue(undefined)
    let finish: (result: Record<string, unknown>) => void = () => {}
    vi.mocked(queryUnrealProject).mockReturnValue(
      new Promise((resolve) => {
        finish = resolve
      })
    )
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()
    await wrapper.get('input[name="unreal-port"]').setValue('55290')
    await wrapper.get('[data-unreal-start]').trigger('click')
    await flushPromises()
    await wrapper.get('[data-unreal-select="ue-test-first"]').trigger('click')
    await wrapper.get('[data-unreal-query]').trigger('click')
    await wrapper.get('[data-unreal-stop]').trigger('click')
    await flushPromises()
    finish({ projectName: 'Stale late response' })
    await flushPromises()

    expect(stopUnrealListener).toHaveBeenCalledTimes(1)
    expect(wrapper.text()).not.toContain('Stale late response')
    expect(wrapper.get('[data-unreal-stop]').attributes('disabled')).toBeDefined()
    wrapper.unmount()
  })

  it('uses the dedicated Unity command with only the explicit target and confirmation', async () => {
    const host = await vi.importActual<typeof import('./engines/unity/commands')>(
      './engines/unity/commands'
    )
    vi.mocked(invoke).mockResolvedValue(unitySnapshot)

    expect(await host.inspectUnity('/tmp/Unity fixture', 55234, true)).toEqual(unitySnapshot)
    expect(invoke).toHaveBeenCalledWith('inspect_unity', {
      projectRoot: '/tmp/Unity fixture',
      port: 55234,
      allowClientTakeover: true
    })
  })

  it('shows the Rust host response and marks unfinished features unavailable', async () => {
    vi.mocked(getHostStatus).mockResolvedValue(actualHostStatus)
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()

    expect(wrapper.text()).toContain('Migration in progress')
    expect(wrapper.text()).toContain('Unreal and Unity have limited read-only queries')
    expect(wrapper.text()).toContain('4321')
    expect(wrapper.text()).toContain('macos')
    expect(wrapper.text()).toContain('aarch64')
    expect(wrapper.text()).toContain('Engine connections')
    expect(wrapper.text()).toContain('Not yet available')
    expect(wrapper.findAll('[data-capability-available="false"]')).toHaveLength(4)
    wrapper.unmount()
  })

  it('shows a host error instead of inventing a status', async () => {
    vi.mocked(getHostStatus).mockRejectedValue(new Error('connection refused'))
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()

    expect(wrapper.text()).toContain('Unable to reach the Rust host')
    expect(wrapper.text()).not.toContain('4321')
    expect(wrapper.findAll('[data-capability-available]')).toHaveLength(0)
    wrapper.unmount()
  })

  it('switches both the migration notice and capability labels to Chinese', async () => {
    vi.mocked(getHostStatus).mockResolvedValue(actualHostStatus)
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()

    expect(wrapper.text()).toContain('Unreal Engine read-only query')
    expect(wrapper.text()).toContain('Unity read-only query')
    expect(wrapper.text()).toContain('Godot editor preview')
    await wrapper.get('button[lang="zh-CN"]').trigger('click')
    expect(wrapper.text()).toContain('迁移进行中')
    expect(wrapper.text()).toContain('引擎连接')
    expect(wrapper.text()).toContain('Unreal Engine 只读查询')
    expect(wrapper.text()).toContain('Unity 只读查询')
    expect(wrapper.text()).toContain('Godot 编辑器预览')
    expect(wrapper.text()).toContain('挤掉该 Unity MCP 端口上现有的客户端')
    wrapper.unmount()
  })

  it('applies and saves the selected light theme', async () => {
    vi.mocked(getHostStatus).mockResolvedValue(actualHostStatus)
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()

    await wrapper.get('button[data-theme-choice="light"]').trigger('click')
    expect(document.documentElement.dataset.theme).toBe('light')
    expect(localStorage.getItem('app-theme')).toBe('light')
    wrapper.unmount()
  })

  it('does not connect by default or without an explicit warning acknowledgement', async () => {
    vi.mocked(getHostStatus).mockResolvedValue(actualHostStatus)
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()

    expect(wrapper.text()).toContain('disconnect an existing client')
    expect(wrapper.text()).toContain('the same path and port may point to a different editor')
    expect(wrapper.text()).toContain('may initialize user-level EditorPrefs')
    expect(wrapper.text()).toContain('Enter a port from 1 to 65535')
    expect((wrapper.get('input[name="unity-project"]').element as HTMLInputElement).value).toBe('')
    expect((wrapper.get('input[name="unity-port"]').element as HTMLInputElement).value).toBe('')
    expect(vi.mocked(inspectUnity)).not.toHaveBeenCalled()
    await wrapper.get('input[name="unity-project"]').setValue('/tmp/Unity fixture')
    await wrapper.get('input[name="unity-port"]').setValue('55234')
    expect(wrapper.get('[data-unity-query]').attributes('disabled')).toBeDefined()
    await wrapper.get('form').trigger('submit')
    expect(vi.mocked(inspectUnity)).not.toHaveBeenCalled()
    wrapper.unmount()
  })

  it('sends the explicitly selected target once after confirmation and renders the response', async () => {
    vi.mocked(getHostStatus).mockResolvedValue(actualHostStatus)
    vi.mocked(inspectUnity).mockResolvedValue(unitySnapshot)
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()

    await wrapper.get('input[name="unity-project"]').setValue('/tmp/Unity fixture')
    await wrapper.get('input[name="unity-port"]').setValue('55234')
    await wrapper.get('button[role="checkbox"]').trigger('click')
    expect(wrapper.get('[data-unity-query]').attributes('disabled')).toBeUndefined()
    await wrapper.get('form').trigger('submit')
    await flushPromises()

    expect(vi.mocked(inspectUnity)).toHaveBeenCalledExactlyOnceWith(
      '/tmp/Unity fixture',
      55234,
      true
    )
    expect(wrapper.text()).toContain('Real fixture project')
    expect(wrapper.text()).toContain('2022.3.fixture')
    expect(wrapper.text()).toContain('Fixture scene')
    expect(wrapper.text()).toContain('Assets/Fixture.unity')
    expect(wrapper.text()).toContain('Fixture platform')
    expect(wrapper.text()).toContain('Fixture pipeline')
    expect(wrapper.get('[data-unity-query]').attributes('disabled')).toBeDefined()
    wrapper.unmount()
  })

  it('invalidates confirmation if the selected target changes or is invalid', async () => {
    vi.mocked(getHostStatus).mockResolvedValue(actualHostStatus)
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()

    await wrapper.get('input[name="unity-project"]').setValue('/tmp/Unity fixture')
    await wrapper.get('input[name="unity-port"]').setValue('55234')
    await wrapper.get('button[role="checkbox"]').trigger('click')
    expect(wrapper.get('[data-unity-query]').attributes('disabled')).toBeUndefined()
    await wrapper.get('input[name="unity-port"]').setValue('55235')
    expect(wrapper.get('[data-unity-query]').attributes('disabled')).toBeDefined()
    await wrapper.get('button[role="checkbox"]').trigger('click')
    await wrapper.get('input[name="unity-port"]').setValue('65536')
    expect(wrapper.get('[data-unity-query]').attributes('disabled')).toBeDefined()
    await wrapper.get('input[name="unity-port"]').setValue('55235')
    await wrapper.get('input[name="unity-project"]').setValue('relative/project')
    await wrapper.get('button[role="checkbox"]').trigger('click')
    expect(wrapper.get('[data-unity-query]').attributes('disabled')).toBeDefined()
    expect(vi.mocked(inspectUnity)).not.toHaveBeenCalled()
    wrapper.unmount()
  })

  it('prevents another submission while the selected connection is in progress', async () => {
    vi.mocked(getHostStatus).mockResolvedValue(actualHostStatus)
    let finish: (result: typeof unitySnapshot) => void = () => {}
    vi.mocked(inspectUnity).mockReturnValue(
      new Promise((resolve) => {
        finish = resolve
      })
    )
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()

    await wrapper.get('input[name="unity-project"]').setValue('/tmp/Unity fixture')
    await wrapper.get('input[name="unity-port"]').setValue('55234')
    await wrapper.get('button[role="checkbox"]').trigger('click')
    await wrapper.get('form').trigger('submit')
    await wrapper.get('form').trigger('submit')
    expect(vi.mocked(inspectUnity)).toHaveBeenCalledTimes(1)
    expect(wrapper.get('input[name="unity-port"]').attributes('disabled')).toBeDefined()
    expect(wrapper.get('button[role="checkbox"]').attributes('disabled')).toBeDefined()

    finish(unitySnapshot)
    await flushPromises()
    expect(wrapper.get('[data-unity-query]').attributes('disabled')).toBeDefined()
    wrapper.unmount()
  })

  it('keeps the Unreal listener usable while a Unity query is pending', async () => {
    vi.mocked(getHostStatus).mockResolvedValue(actualHostStatus)
    vi.mocked(startUnrealListener).mockResolvedValue({ address: '127.0.0.1', port: 55290 })
    vi.mocked(listUnrealInstances).mockResolvedValue([unrealInstance])
    vi.mocked(stopUnrealListener).mockResolvedValue(undefined)
    let finish: (result: typeof unitySnapshot) => void = () => {}
    vi.mocked(inspectUnity).mockReturnValue(
      new Promise((resolve) => {
        finish = resolve
      })
    )
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()
    await wrapper.get('input[name="unity-project"]').setValue('/tmp/Unity fixture')
    await wrapper.get('input[name="unity-port"]').setValue('55234')
    await wrapper.get('button[role="checkbox"]').trigger('click')
    await wrapper.get('form').trigger('submit')
    expect(wrapper.get('[data-unity-query]').attributes('disabled')).toBeDefined()

    await wrapper.get('input[name="unreal-port"]').setValue('55290')
    await wrapper.get('[data-unreal-start]').trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('Editor A')
    await wrapper.get('[data-unreal-stop]').trigger('click')
    await flushPromises()
    expect(stopUnrealListener).toHaveBeenCalledTimes(1)
    expect(inspectUnity).toHaveBeenCalledTimes(1)

    finish(unitySnapshot)
    await flushPromises()
    expect(wrapper.text()).toContain('Fixture scene')
    wrapper.unmount()
  })

  it('keeps Unity authorization usable while an Unreal query is pending', async () => {
    vi.mocked(getHostStatus).mockResolvedValue(actualHostStatus)
    vi.mocked(startUnrealListener).mockResolvedValue({ address: '127.0.0.1', port: 55290 })
    vi.mocked(listUnrealInstances).mockResolvedValue([unrealInstance])
    vi.mocked(inspectUnity).mockResolvedValue(unitySnapshot)
    let finish: (result: Record<string, unknown>) => void = () => {}
    vi.mocked(queryUnrealProject).mockReturnValue(
      new Promise((resolve) => {
        finish = resolve
      })
    )
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()
    await wrapper.get('input[name="unreal-port"]').setValue('55290')
    await wrapper.get('[data-unreal-start]').trigger('click')
    await flushPromises()
    await wrapper.get('[data-unreal-select="ue-test-first"]').trigger('click')
    await wrapper.get('[data-unreal-query]').trigger('click')

    await wrapper.get('input[name="unity-project"]').setValue('/tmp/Unity fixture')
    await wrapper.get('input[name="unity-port"]').setValue('55234')
    await wrapper.get('button[role="checkbox"]').trigger('click')
    expect(wrapper.get('[data-unity-query]').attributes('disabled')).toBeUndefined()
    await wrapper.get('form').trigger('submit')
    await flushPromises()
    expect(inspectUnity).toHaveBeenCalledExactlyOnceWith('/tmp/Unity fixture', 55234, true)
    expect(wrapper.text()).toContain('Fixture scene')

    finish({ projectName: 'Unreal result' })
    await flushPromises()
    expect(wrapper.text()).toContain('Unreal result')
    wrapper.unmount()
  })

  it('passes the host OS to Unity path validation', async () => {
    vi.mocked(getHostStatus).mockResolvedValue({ ...actualHostStatus, os: 'windows' })
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()
    await wrapper.get('input[name="unity-project"]').setValue('C:\\Projects\\Fixture')
    await wrapper.get('input[name="unity-port"]').setValue('55234')
    await wrapper.get('button[role="checkbox"]').trigger('click')
    expect(wrapper.get('[data-unity-query]').attributes('disabled')).toBeUndefined()
    await wrapper.get('input[name="unity-project"]').setValue('/tmp/Unity fixture')
    await wrapper.get('button[role="checkbox"]').trigger('click')
    expect(wrapper.get('[data-unity-query]').attributes('disabled')).toBeDefined()
    expect(inspectUnity).not.toHaveBeenCalled()
    wrapper.unmount()
  })

  it('retains the POSIX path fallback for a non-Windows host status', async () => {
    vi.mocked(getHostStatus).mockResolvedValue({ ...actualHostStatus, os: '' })
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()
    await wrapper.get('input[name="unity-project"]').setValue('/tmp/Unity fixture')
    await wrapper.get('input[name="unity-port"]').setValue('55234')
    await wrapper.get('button[role="checkbox"]').trigger('click')
    expect(wrapper.get('[data-unity-query]').attributes('disabled')).toBeUndefined()
    wrapper.unmount()
  })

  it('shows an empty scene path without assuming why it is empty', async () => {
    vi.mocked(getHostStatus).mockResolvedValue(actualHostStatus)
    vi.mocked(inspectUnity).mockResolvedValue({
      ...unitySnapshot,
      scene: { ...unitySnapshot.scene, name: '', path: '', isLoaded: false }
    })
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()

    await wrapper.get('input[name="unity-project"]').setValue('/tmp/Unity fixture')
    await wrapper.get('input[name="unity-port"]').setValue('55234')
    await wrapper.get('button[role="checkbox"]').trigger('click')
    await wrapper.get('form').trigger('submit')
    await flushPromises()

    expect(wrapper.text()).toContain('Empty')
    expect(wrapper.text()).not.toContain('Unnamed')
    expect(wrapper.text()).not.toContain('Not saved yet')
    wrapper.unmount()
  })

  it('displays the real Unity error without displaying a stale result', async () => {
    vi.mocked(getHostStatus).mockResolvedValue(actualHostStatus)
    vi.mocked(inspectUnity)
      .mockResolvedValueOnce(unitySnapshot)
      .mockRejectedValueOnce('Unity project mismatch: expected A, received B')
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await flushPromises()

    await wrapper.get('input[name="unity-project"]').setValue('/tmp/Unity fixture')
    await wrapper.get('input[name="unity-port"]').setValue('55234')
    await wrapper.get('button[role="checkbox"]').trigger('click')
    expect(wrapper.get('[data-unity-query]').attributes('disabled')).toBeUndefined()
    await wrapper.get('form').trigger('submit')
    await flushPromises()
    expect(wrapper.text()).toContain('Fixture scene')

    await wrapper.get('button[role="checkbox"]').trigger('click')
    await wrapper.get('form').trigger('submit')
    await flushPromises()

    expect(wrapper.text()).toContain('Unity project mismatch: expected A, received B')
    expect(wrapper.text()).not.toContain('Fixture scene')
    expect(wrapper.get('[data-unity-query]').attributes('disabled')).toBeDefined()
    wrapper.unmount()
  })
})
