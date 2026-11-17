import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process'
import { createServer, type Server } from 'node:http'
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { resolve } from 'node:path'
import { createInterface } from 'node:readline'
import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, expect, it, vi } from 'vitest'
import AgentPanel from '../src/engines/godot/AgentPanel.vue'
import { createAppI18n } from '../src/i18n'

type Reply = { id: number; ok: boolean; value?: unknown; error?: unknown; ready?: boolean }
let callRust: (command: string, args?: Record<string, unknown>) => Promise<unknown>
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (command: string, args?: Record<string, unknown>) => callRust(command, args)
}))
let child: ChildProcessWithoutNullStreams | undefined
let project: string | undefined
let server: Server | undefined

afterEach(async () => {
  child?.kill()
  child = undefined
  if (server) await new Promise<void>((done) => server!.close(() => done()))
  server = undefined
  if (project) await rm(project, { recursive: true, force: true })
  project = undefined
})
function binary(name: string): string {
  return resolve(
    process.cwd(),
    process.env.CARGO_TARGET_DIR ?? 'src-tauri/target',
    `debug/examples/${name}${process.platform === 'win32' ? '.exe' : ''}`
  )
}
async function harness(): Promise<void> {
  child = spawn(binary('godot_agent_ui_harness'), [], { stdio: 'pipe' })
  const pending = new Map<
    number,
    { resolve: (value: unknown) => void; reject: (error: unknown) => void }
  >()
  let next = 0
  let readyResolve: () => void = () => {}
  let readyReject: (error: Error) => void = () => {}
  const ready = new Promise<void>((resolveReady, rejectReady) => {
    readyResolve = resolveReady
    readyReject = rejectReady
  })
  createInterface({ input: child.stdout }).on('line', (line) => {
    const reply: Reply = JSON.parse(line)
    if (reply.ready) {
      readyResolve()
      return
    }
    const waiter = pending.get(reply.id)
    if (!waiter) return
    pending.delete(reply.id)
    if (reply.ok) waiter.resolve(reply.value)
    else waiter.reject(reply.error)
  })
  child.on('error', readyReject)
  child.on('exit', () => {
    const error = new Error('Rust test harness exited')
    readyReject(error)
    for (const waiter of pending.values()) waiter.reject(error)
    pending.clear()
  })
  callRust = (command, args = {}) =>
    new Promise((resolveReply, reject) => {
      const id = ++next
      pending.set(id, { resolve: resolveReply, reject })
      child!.stdin.write(`${JSON.stringify({ id, command, args })}\n`)
    })
  await ready
}
async function eventually(assertion: () => void | Promise<void>, timeout = 5000): Promise<void> {
  const deadline = Date.now() + timeout
  for (;;) {
    await flushPromises()
    try {
      await assertion()
      return
    } catch (error) {
      if (Date.now() >= deadline) throw error
      await new Promise((done) => setTimeout(done, 20))
    }
  }
}

it('uses a fixed Godot target through real Rust commands, local streaming model, cancellation and drain', async () => {
  project = await mkdtemp(resolve(tmpdir(), 'uebox-agent-ui-'))
  await writeFile(resolve(project, 'project.godot'), 'config_version=5\n')
  const bodies: string[] = []
  const authorizations: (string | undefined)[] = []
  server = createServer((request, response) => {
    let body = ''
    request.setEncoding('utf8')
    request.on('data', (part: string) => {
      body += part
    })
    request.on('end', () => {
      bodies.push(body)
      authorizations.push(request.headers.authorization)
      if (bodies.length === 5) {
        response.writeHead(401, { 'Content-Type': 'text/plain', Connection: 'close' })
        response.end('synthetic-fixture-key is an upstream error body')
        return
      }
      response.writeHead(200, { 'Content-Type': 'text/event-stream', Connection: 'close' })
      const event = (delta: unknown, finish: string | null): string =>
        `data: ${JSON.stringify({ choices: [{ index: 0, delta, finish_reason: finish }] })}\n\n`
      if (bodies.length === 7) {
        response.write(event({ content: 'waiting' }, null))
      } else if (
        bodies.length === 1 ||
        bodies.length === 3 ||
        bodies.length === 6 ||
        bodies.length === 8
      ) {
        const names =
          bodies.length === 1 ? ['godot_project_info', 'godot_scene_tree'] : ['godot_project_info']
        const calls = names.map((name, index) => ({
          index,
          id: `call-${bodies.length}-${index}`,
          type: 'function',
          function: { name, arguments: '{}' }
        }))
        response.end(event({ tool_calls: calls }, 'tool_calls') + 'data: [DONE]\n\n')
      } else if (bodies.length === 2) {
        response.write(event({ content: 'First ' }, null))
        setTimeout(
          () => response.end(event({ content: 'answer' }, 'stop') + 'data: [DONE]\n\n'),
          300
        )
      } else response.end(event({ content: 'Follow-up answer' }, 'stop') + 'data: [DONE]\n\n')
    })
  })
  await new Promise<void>((done, reject) => {
    server!.once('error', reject)
    server!.listen(0, '127.0.0.1', done)
  })
  const address = server.address()
  if (!address || typeof address === 'string') throw new Error('No fixture port')
  await harness()
  const launched = (await callRust('launch_godot', {
    executable: binary('godot_editor_fixture'),
    project
  })) as { sessionId: string }
  await eventually(async () => {
    expect(await readFile(resolve(project!, 'fixture-ready'), 'utf8')).toMatch(/^\d+$/)
  })
  const editorPid = Number(await readFile(resolve(project, 'fixture-ready'), 'utf8'))
  const connected = (await callRust('connect_godot', { sessionId: launched.sessionId })) as {
    name: string
  }
  expect(connected.name).toBe('Tauri agent fixture project')
  let createdId = ''
  const callOwned = callRust
  callRust = async (command, args) => {
    const result = await callOwned(command, args)
    if (command === 'create_godot_agent') createdId = (result as { agentId: string }).agentId
    return result
  }
  const wrapper = mount(AgentPanel, {
    props: { sessionId: launched.sessionId, ready: true },
    global: { plugins: [createAppI18n('en-US')] }
  })
  try {
    await wrapper
      .get('input[name="agent-endpoint"]')
      .setValue(`http://127.0.0.1:${address.port}/v1/chat/completions`)
    await wrapper.get('input[name="agent-model"]').setValue('fixture-model')
    await wrapper.get('input[name="agent-credential"]').setValue('synthetic-fixture-key')
    await wrapper.get('form.agent-form').trigger('submit')
    await eventually(() =>
      expect(wrapper.find('textarea[name="agent-message"]').exists()).toBe(true)
    )
    expect(createdId).toBeTruthy()
    await expect(
      callRust('fixture_other_start', { agentId: createdId, text: 'forbidden' })
    ).rejects.toContain('agent_unavailable')
    await expect(
      callRust('fixture_other_poll', { agentId: createdId, runId: 1 })
    ).rejects.toContain('agent_unavailable')
    await expect(
      callRust('fixture_other_cancel', { agentId: createdId, runId: 1 })
    ).rejects.toContain('agent_unavailable')
    await expect(callRust('fixture_other_close', { agentId: createdId })).rejects.toContain(
      'agent_unavailable'
    )
    expect(bodies).toHaveLength(0)
    expect((wrapper.get('input[name="agent-credential"]').element as HTMLInputElement).value).toBe(
      ''
    )
    await wrapper.get('textarea[name="agent-message"]').setValue('Read the project and scene')
    await wrapper.findAll('form.agent-form')[1].trigger('submit')
    await eventually(() => expect(wrapper.text()).toContain('First '))
    expect(wrapper.text()).not.toContain('First answer')
    await eventually(() => expect(wrapper.text()).toContain('First answer'))
    await eventually(() => expect(wrapper.text()).toContain('Completed'))
    expect(bodies).toHaveLength(2)
    expect(bodies[1]).toContain('Tauri agent fixture project')
    expect(bodies[1]).toContain('FixtureWorld')
    expect(bodies[1]).toContain('truncated')
    expect(bodies[1]).not.toContain(project)
    for (const body of bodies) {
      expect(body).not.toContain('synthetic-fixture-key')
      expect(body).not.toMatch(/[0-9a-f]{32,}/)
    }
    expect(authorizations).toEqual(['Bearer synthetic-fixture-key', 'Bearer synthetic-fixture-key'])

    await writeFile(resolve(project, 'fixture-hold'), 'hold')
    await wrapper.get('textarea[name="agent-message"]').setValue('Read again')
    await wrapper.findAll('form.agent-form')[1].trigger('submit')
    await eventually(async () => {
      expect(await readFile(resolve(project!, 'fixture-inflight'), 'utf8')).toBe('yes')
    })
    await wrapper.get('[data-agent-cancel]').trigger('click')
    await eventually(() => expect(wrapper.text()).toContain('Draining'))
    expect(wrapper.get('[data-agent-send]').attributes('disabled')).toBeDefined()
    await writeFile(resolve(project, 'fixture-release'), 'release')
    await eventually(() => expect(wrapper.text()).toContain('Cancelled'))
    expect(bodies).toHaveLength(3)
    await wrapper.get('textarea[name="agent-message"]').setValue('Continue')
    await wrapper.findAll('form.agent-form')[1].trigger('submit')
    await eventually(() => expect(wrapper.text()).toContain('Follow-up answer'))
    await eventually(() => expect(wrapper.text()).toContain('Completed'))
    expect(bodies).toHaveLength(4)

    await wrapper.get('textarea[name="agent-message"]').setValue('Check an upstream error')
    await wrapper.findAll('form.agent-form')[1].trigger('submit')
    await eventually(() => expect(wrapper.text()).toContain('unsuccessful HTTP status'))
    expect(wrapper.text()).not.toContain('synthetic-fixture-key')
    expect(bodies).toHaveLength(5)

    await rm(resolve(project, 'fixture-release'))
    await rm(resolve(project, 'fixture-inflight'))
    await wrapper.get('textarea[name="agent-message"]').setValue('Read while releasing')
    await wrapper.findAll('form.agent-form')[1].trigger('submit')
    await eventually(async () => {
      expect(await readFile(resolve(project!, 'fixture-inflight'), 'utf8')).toBe('yes')
    })
    const releasing = callRust('release_godot_session', { sessionId: launched.sessionId })
    await wrapper.setProps({ ready: false })
    let released = false
    void releasing.then(() => {
      released = true
    })
    await new Promise((done) => setTimeout(done, 150))
    expect(released).toBe(false)
    await writeFile(resolve(project, 'fixture-release'), 'release')
    await releasing
    expect(bodies).toHaveLength(6)
    await expect(
      callRust('start_godot_agent', { agentId: createdId, text: 'no' })
    ).rejects.toContain('agent_unavailable')
    expect(() => process.kill(editorPid, 0)).not.toThrow()

    await rm(project, { recursive: true, force: true })
    project = await mkdtemp(resolve(tmpdir(), 'uebox-agent-ui-'))
    await writeFile(resolve(project, 'project.godot'), 'config_version=5\n')
    const second = (await callRust('launch_godot', {
      executable: binary('godot_editor_fixture'),
      project
    })) as { sessionId: string }
    await eventually(async () => {
      expect(await readFile(resolve(project!, 'fixture-ready'), 'utf8')).toMatch(/^\d+$/)
    })
    const secondPid = Number(await readFile(resolve(project, 'fixture-ready'), 'utf8'))
    await callRust('connect_godot', { sessionId: second.sessionId })
    await wrapper.setProps({ sessionId: second.sessionId, ready: true })
    await wrapper.get('form.agent-form').trigger('submit')
    await eventually(() =>
      expect(wrapper.find('textarea[name="agent-message"]').exists()).toBe(true)
    )
    const secondAgent = createdId
    await wrapper.get('textarea[name="agent-message"]').setValue('Wait for editor exit')
    await wrapper.findAll('form.agent-form')[1].trigger('submit')
    await eventually(() => expect(bodies).toHaveLength(7))
    process.kill(secondPid, 'SIGTERM') // only the fixture process created by this test
    await eventually(() =>
      expect(wrapper.text()).toContain('selected Godot connection is no longer available')
    )
    await expect(
      callRust('start_godot_agent', { agentId: secondAgent, text: 'no' })
    ).rejects.toContain('agent_unavailable')
    expect(bodies).toHaveLength(7)
    await callRust('release_godot_session', { sessionId: second.sessionId })

    await wrapper.setProps({ ready: false })
    await rm(project, { recursive: true, force: true })
    project = await mkdtemp(resolve(tmpdir(), 'uebox-agent-ui-'))
    await writeFile(resolve(project, 'project.godot'), 'config_version=5\n')
    const third = (await callRust('launch_godot', {
      executable: binary('godot_editor_fixture'),
      project
    })) as { sessionId: string }
    await eventually(async () => {
      expect(await readFile(resolve(project!, 'fixture-ready'), 'utf8')).toMatch(/^\d+$/)
    })
    await callRust('connect_godot', { sessionId: third.sessionId })
    await wrapper.setProps({ sessionId: third.sessionId, ready: true })
    await wrapper.get('form.agent-form').trigger('submit')
    await eventually(() =>
      expect(wrapper.find('textarea[name="agent-message"]').exists()).toBe(true)
    )
    await writeFile(resolve(project, 'fixture-disconnect'), 'disconnect')
    await wrapper.get('textarea[name="agent-message"]').setValue('Read after bridge disconnect')
    await wrapper.findAll('form.agent-form')[1].trigger('submit')
    await eventually(() =>
      expect(wrapper.text()).toContain('selected Godot connection is no longer available')
    )
    await new Promise((done) => setTimeout(done, 300))
    expect(bodies).toHaveLength(8)
    await callRust('release_godot_session', { sessionId: third.sessionId })
  } finally {
    wrapper.unmount()
  }
}, 25000)
