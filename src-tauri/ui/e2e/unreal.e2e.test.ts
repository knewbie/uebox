import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process'
import { createServer } from 'node:net'
import { createInterface } from 'node:readline'
import { resolve } from 'node:path'
import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, expect, it, vi } from 'vitest'
import App from '../src/App.vue'
import { createAppI18n } from '../src/i18n'

type Reply = { id: number; ok: boolean; value?: unknown; error?: unknown; ready?: boolean }
let callRust: (command: string, args?: Record<string, unknown>) => Promise<unknown>
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (command: string, args?: Record<string, unknown>) => callRust(command, args)
}))

let child: ChildProcessWithoutNullStreams | undefined
afterEach(() => {
  child?.kill()
  child = undefined
})

async function unusedPort(): Promise<number> {
  const server = createServer()
  await new Promise<void>((resolveListen, reject) => {
    server.once('error', reject)
    server.listen(0, '127.0.0.1', () => resolveListen())
  })
  const address = server.address()
  if (!address || typeof address === 'string') throw new Error('No TCP port')
  const port = address.port
  expect(port).not.toBe(17860)
  expect(port).not.toBe(6400)
  await new Promise<void>((resolveClose) => server.close(() => resolveClose()))
  return port
}

async function startHarness(): Promise<void> {
  const binary = resolve(
    process.cwd(),
    process.env.CARGO_TARGET_DIR ?? 'src-tauri/target',
    'debug/examples/unreal_ui_harness' + (process.platform === 'win32' ? '.exe' : '')
  )
  child = spawn(binary, [], { stdio: 'pipe' })
  const pending = new Map<
    number,
    { resolve: (value: unknown) => void; reject: (error: unknown) => void }
  >()
  let nextId = 0
  let readyResolve: () => void = () => {}
  let readyReject: (error: Error) => void = () => {}
  const ready = new Promise<void>((resolveReady, rejectReady) => {
    readyResolve = resolveReady
    readyReject = rejectReady
  })
  const lines = createInterface({ input: child.stdout })
  lines.on('line', (line) => {
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
  child.on('exit', (code) => {
    const error = new Error(`Rust integration harness exited: ${code}`)
    readyReject(error)
    for (const waiter of pending.values()) waiter.reject(error)
    pending.clear()
  })
  callRust = (command, args = {}) =>
    new Promise((resolveReply, reject) => {
      const id = ++nextId
      pending.set(id, { resolve: resolveReply, reject })
      child!.stdin.write(`${JSON.stringify({ id, command, args })}\n`)
    })
  await ready
}

async function eventually(assertion: () => void, timeout = 4000): Promise<void> {
  const deadline = Date.now() + timeout
  for (;;) {
    await flushPromises()
    try {
      assertion()
      return
    } catch (error) {
      if (Date.now() >= deadline) throw error
      await new Promise((resolveWait) => setTimeout(resolveWait, 10))
    }
  }
}

it('resolves the fixture from CARGO_TARGET_DIR when it is set', async () => {
  const previous = process.env.CARGO_TARGET_DIR
  process.env.CARGO_TARGET_DIR = resolve(process.cwd(), 'src-tauri/target', `missing-${Date.now()}`)
  try {
    await expect(startHarness()).rejects.toMatchObject({ code: 'ENOENT' })
  } finally {
    if (previous === undefined) delete process.env.CARGO_TARGET_DIR
    else process.env.CARGO_TARGET_DIR = previous
  }
})

it('connects the Vue controls to real Rust commands and a real loopback WebSocket fixture', async () => {
  const port = await unusedPort()
  await startHarness()
  const wrapper = mount(App, {
    attachTo: document.body,
    global: { plugins: [createAppI18n('en-US')] }
  })
  await eventually(() => expect(wrapper.text()).toContain('Local host'))
  await eventually(() =>
    expect(wrapper.get('[data-unreal-start]').attributes('disabled')).toBeUndefined()
  )

  await wrapper.get('input[name="unreal-port"]').setValue(String(port))
  await wrapper.get('[data-unreal-start]').trigger('click')
  await eventually(() => expect(wrapper.text()).toContain(`127.0.0.1:${port}`))
  await callRust('fixture_connect', { port })
  await wrapper.get('[data-unreal-refresh]').trigger('click')
  await eventually(() => expect(wrapper.text()).toContain('E2E fixture editor'))
  expect(wrapper.get('[data-unreal-query]').attributes('disabled')).toBeDefined()
  await wrapper.get('[data-unreal-select]').trigger('click')
  await wrapper.get('[data-unreal-query]').trigger('click')
  await eventually(() => expect(wrapper.text()).toContain('E2E actual project result'))
  await wrapper.get('[data-unreal-stop]').trigger('click')
  await eventually(() => expect(wrapper.text()).toContain('Listener not started'))
  wrapper.unmount()
})

it('shows the actual Rust bind error when the selected port is occupied', async () => {
  const server = createServer()
  await new Promise<void>((resolveListen, reject) => {
    server.once('error', reject)
    server.listen(0, '127.0.0.1', () => resolveListen())
  })
  const address = server.address()
  if (!address || typeof address === 'string') throw new Error('No TCP port')
  expect(address.port).not.toBe(17860)
  expect(address.port).not.toBe(6400)
  try {
    await startHarness()
    const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
    await eventually(() => expect(wrapper.text()).toContain('Local host'))
    await eventually(() =>
      expect(wrapper.get('[data-unreal-start]').attributes('disabled')).toBeUndefined()
    )
    await wrapper.get('input[name="unreal-port"]').setValue(String(address.port))
    await wrapper.get('[data-unreal-start]').trigger('click')
    await eventually(() => expect(wrapper.text()).toContain('Failed to start Unreal listener:'))
    expect(wrapper.text()).toMatch(/os error \d+/)
    expect(wrapper.text()).toContain('Listener not started')
    wrapper.unmount()
  } finally {
    await new Promise<void>((resolveClose) => server.close(() => resolveClose()))
  }
})

it('shows the real project query timeout from the Rust command', async () => {
  const port = await unusedPort()
  await startHarness()
  const wrapper = mount(App, { global: { plugins: [createAppI18n('en-US')] } })
  await eventually(() => expect(wrapper.text()).toContain('Local host'))
  await eventually(() =>
    expect(wrapper.get('[data-unreal-start]').attributes('disabled')).toBeUndefined()
  )
  await wrapper.get('input[name="unreal-port"]').setValue(String(port))
  await wrapper.get('[data-unreal-start]').trigger('click')
  await eventually(() => expect(wrapper.text()).toContain(`127.0.0.1:${port}`))
  await callRust('fixture_connect', { port, respond: false })
  await wrapper.get('[data-unreal-refresh]').trigger('click')
  await eventually(() => expect(wrapper.text()).toContain('E2E fixture editor'))
  await wrapper.get('[data-unreal-select]').trigger('click')
  await wrapper.get('[data-unreal-query]').trigger('click')
  await eventually(
    () => expect(wrapper.text()).toContain('request timed out; its outcome is unknown'),
    14000
  )
  expect(wrapper.text()).not.toContain('The previous selection is no longer valid')
  expect(wrapper.get('[data-unreal-query]').attributes('disabled')).toBeDefined()
  await wrapper.get('[data-unreal-stop]').trigger('click')
  await eventually(() => expect(wrapper.text()).toContain('Listener not started'))
  wrapper.unmount()
}, 20000)
