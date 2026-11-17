import { existsSync, readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { describe, expect, it } from 'vitest'

const root = resolve(process.cwd(), 'src-tauri')

function source(path: string): string {
  const fullPath = resolve(root, path)
  expect(existsSync(fullPath), `Missing independent engine module: ${path}`).toBe(true)
  return readFileSync(fullPath, 'utf8')
}

function rendererImports(content: string): string[] {
  const modules = [...content.matchAll(/\b(?:from\s*|import\s*(?:\(\s*)?)(['"])([^'"]+)\1/g)].map(
    (match) => match[2]
  )
  const styles = [...content.matchAll(/<style\b[^>]*\bsrc=['"]([^'"]+)['"]/g)].map(
    (match) => match[1]
  )
  return [...modules, ...styles]
}

describe('Tauri engine ownership boundaries', () => {
  it('keeps Unity Rust and renderer modules independent of the other engines', () => {
    expect(source('src/unity.rs')).not.toMatch(
      /\b(?:uebox_engine_unreal|uebox_godot_session|(?:crate|super)::(?:unreal|godot)|(?:unreal|godot)::)/
    )
    for (const path of [
      'ui/src/engines/unity/UnityPanel.vue',
      'ui/src/engines/unity/commands.ts'
    ]) {
      expect(rendererImports(source(path)), path).not.toEqual(
        expect.arrayContaining([expect.stringMatching(/(?:^|\/)(?:unreal|godot)(?:\/|$)/i)])
      )
    }
  })

  it('keeps Unreal Rust and renderer modules independent of the other engines', () => {
    expect(source('src/unreal.rs')).not.toMatch(
      /\b(?:uebox_engine_unity|uebox_godot_session|(?:crate|super)::(?:unity|godot)|(?:unity|godot)::)/
    )
    for (const path of [
      'ui/src/engines/unreal/UnrealPanel.vue',
      'ui/src/engines/unreal/commands.ts'
    ]) {
      expect(rendererImports(source(path)), path).not.toEqual(
        expect.arrayContaining([expect.stringMatching(/(?:^|\/)(?:unity|godot)(?:\/|$)/i)])
      )
    }
  })

  it('keeps Godot Rust and renderer modules independent of the other engines', () => {
    expect(source('src/godot.rs')).not.toMatch(
      /\b(?:uebox_engine_unity|uebox_engine_unreal|(?:crate|super)::(?:unity|unreal)|(?:unity|unreal)::)/
    )
    for (const path of [
      'ui/src/engines/godot/GodotPanel.vue',
      'ui/src/engines/godot/commands.ts'
    ]) {
      expect(rendererImports(source(path)), path).not.toEqual(
        expect.arrayContaining([expect.stringMatching(/(?:^|\/)(?:unity|unreal)(?:\/|$)/i)])
      )
    }
  })

  it('keeps engine implementation out of the shared entry points', () => {
    const rustEntry = source('src/lib.rs')
    expect(rustEntry).toContain('mod unity;')
    expect(rustEntry).toContain('mod unreal;')
    expect(rustEntry).toContain('mod godot;')
    expect(rustEntry).not.toMatch(
      /(?:struct|fn)\s+(?:UnityQueries|UnrealListenerState|GodotSessions|inspect_unity|start_unreal_listener|launch_godot)/
    )

    const appEntry = source('ui/src/App.vue')
    expect(appEntry).toContain('<UnityPanel')
    expect(appEntry).toContain('<UnrealPanel')
    expect(appEntry).toContain('<GodotPanel')
    expect(appEntry).not.toMatch(
      /inspectUnity|startUnrealListener|unityTakeoverAllowed|unrealSelected|launchGodot/
    )

    const hostEntry = source('ui/src/host.ts')
    expect(rendererImports(hostEntry)).not.toEqual(
      expect.arrayContaining([
        expect.stringMatching(/(?:^|\/)engines\/(?:unity|unreal|godot)(?:\/|$)/i)
      ])
    )
    expect(hostEntry).not.toMatch(/inspectUnity|startUnrealListener|queryUnrealProject|launchGodot/)
  })
})
