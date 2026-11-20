#!/usr/bin/env node
// Package the manually invoked Rust console separately from the Tauri application.
import { execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync, readFileSync, renameSync, rmSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import AdmZip from 'adm-zip'

const root = fileURLToPath(new URL('../', import.meta.url))
const consoleRoot = join(root, 'crates', 'uebox-cli')
const cargoTarget = resolve(root, process.env.CARGO_TARGET_DIR || join(consoleRoot, 'target'))
const output = resolve(root, process.argv[2] || join('dist', 'native-cli'))
const executable = process.platform === 'win32' ? 'uebox.exe' : 'uebox'

if (process.argv.length > 3 || !['darwin', 'linux', 'win32'].includes(process.platform)) {
  console.error('Usage: pnpm tauri:cli:build [output-directory]')
  process.exit(1)
}

const target = /^host: ([a-z0-9_-]+)$/m.exec(
  execFileSync('rustc', ['--version', '--verbose'], { encoding: 'utf8' })
)?.[1]
if (!target) throw new Error('Rust did not report a native host target')

execFileSync(
  'cargo',
  [
    'build',
    '--locked',
    '--release',
    '--target',
    target,
    '--manifest-path',
    join(consoleRoot, 'Cargo.toml'),
    '--target-dir',
    cargoTarget
  ],
  { cwd: root, stdio: 'inherit' }
)

const binary = join(cargoTarget, target, 'release', executable)
const versionReply = JSON.parse(
  execFileSync(binary, ['--version', '--json'], { encoding: 'utf8', env: {} })
)
const version = versionReply.data?.version
if (typeof version !== 'string' || !/^\d+\.\d+\.\d+(?:[-+][a-zA-Z0-9.-]+)?$/.test(version)) {
  throw new Error('The built native CLI did not report a version')
}

mkdirSync(output, { recursive: true })
const staging = mkdtempSync(join(output, '.uebox-cli-'))
try {
  const archiveName = `uebox-cli-${version}-${target}.zip`
  const archive = join(staging, archiveName)
  const zip = new AdmZip()
  zip.addFile(executable, readFileSync(binary), '', 0o100755)
  zip.addFile('README.md', readFileSync(join(consoleRoot, 'README.md')), '', 0o100644)
  zip.addFile('LICENSE', readFileSync(join(root, 'LICENSE')), '', 0o100644)
  zip.writeZip(archive)
  const published = join(output, archiveName)
  renameSync(archive, published)
  console.log(published)
} finally {
  rmSync(staging, { recursive: true, force: true })
}
