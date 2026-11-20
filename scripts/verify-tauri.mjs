#!/usr/bin/env node
/**
 * Focused verification for the in-progress Tauri/Rust migration.
 * The optional real Godot editor test uses disposable projects only.
 *
 * pnpm verify:tauri
 * GODOT_BIN=/path/to/Godot pnpm verify:tauri --real-godot
 */
import { spawnSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { dirname, resolve } from 'node:path'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const realGodot = process.argv.slice(2).includes('--real-godot')
if (realGodot && !process.env.GODOT_BIN) {
  console.error('GODOT_BIN must identify the editor for --real-godot.')
  process.exit(1)
}
if (realGodot) {
  process.env.UEBOX_TEST_GODOT_BIN = process.env.GODOT_BIN
}

const crates = [
  'crates/uebox-engine-core/Cargo.toml',
  'crates/uebox-engine-unreal/Cargo.toml',
  'crates/uebox-engine-unity/Cargo.toml',
  'crates/uebox-engine-godot/Cargo.toml',
  'crates/uebox-godot-session/Cargo.toml',
  'crates/uebox-agent-core/Cargo.toml',
  'crates/uebox-agent-engine-tools/Cargo.toml',
  'crates/uebox-agent-unity-tools/Cargo.toml',
  'crates/uebox-agent-godot-tools/Cargo.toml',
  'crates/uebox-data/Cargo.toml',
  'crates/uebox-project-library/Cargo.toml',
  'crates/uebox-project-collections/Cargo.toml',
  'crates/uebox-chat-history/Cargo.toml',
  'crates/uebox-legacy-history/Cargo.toml',
  'crates/uebox-credentials/Cargo.toml',
  'crates/uebox-cli/Cargo.toml',
  'src-tauri/Cargo.toml'
]

function run(command, args) {
  console.log('\n> ' + [command, ...args].join(' '))
  const result = spawnSync(command, args, {
    cwd: root,
    stdio: 'inherit',
    // pnpm is a .cmd shim on Windows; arguments below are fixed literals.
    shell: process.platform === 'win32' && command === 'pnpm'
  })
  if (result.error) {
    console.error(result.error.message)
    process.exit(1)
  }
  if (result.status !== 0) process.exit(result.status ?? 1)
}

run('pnpm', ['tauri:ui:test'])
run('pnpm', ['exec', 'vue-tsc', '--noEmit', '-p', 'src-tauri/ui/tsconfig.json'])
run('pnpm', ['tauri:ui:build'])

for (const manifest of crates) {
  run('cargo', ['fmt', '--manifest-path', manifest, '--', '--check'])
  const featureArgs = []
  if (realGodot && manifest === 'crates/uebox-engine-godot/Cargo.toml') {
    featureArgs.push('--features', 'real-editor')
  }
  if (
    manifest === 'crates/uebox-godot-session/Cargo.toml' ||
    manifest === 'crates/uebox-agent-godot-tools/Cargo.toml'
  ) {
    featureArgs.push('--features', realGodot ? 'test-fixtures,real-editor' : 'test-fixtures')
  }
  if (realGodot && manifest === 'src-tauri/Cargo.toml') {
    featureArgs.push('--features', 'godot-real-editor-test')
  }
  run('cargo', ['test', '--locked', '--manifest-path', manifest, ...featureArgs])
  run('cargo', [
    'clippy',
    '--locked',
    '--manifest-path',
    manifest,
    '--all-targets',
    ...featureArgs,
    '--',
    '-D',
    'warnings'
  ])
}

run('cargo', [
  'check',
  '--locked',
  '--manifest-path',
  'crates/uebox-agent-engine-tools/Cargo.toml',
  '--no-default-features'
])
for (const engine of ['unity', 'unreal', 'godot']) {
  run('cargo', [
    'check',
    '--locked',
    '--manifest-path',
    'src-tauri/Cargo.toml',
    '--no-default-features',
    '--features',
    engine
  ])
}
run('cargo', [
  'build',
  '--locked',
  '--manifest-path',
  'src-tauri/Cargo.toml',
  '--example',
  'unreal_ui_harness',
  '--example',
  'godot_agent_ui_harness',
  '--example',
  'godot_editor_fixture'
])
run('pnpm', ['exec', 'vitest', 'run', '--config', 'src-tauri/ui/vitest.e2e.config.ts'])
