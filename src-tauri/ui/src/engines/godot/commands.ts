import { invoke } from '@tauri-apps/api/core'

export type GodotState = 'starting' | 'ready' | 'disconnected' | 'startup_expired' | 'exited'

export interface GodotLaunch {
  sessionId: string
  state: 'starting'
}

export interface GodotProject {
  name: string
  godotVersion: string
}

export interface GodotScene {
  scenePath: string | null
  nodes: { path: string; name: string; class: string }[]
  truncated: boolean
}

export interface GodotStatus {
  state: GodotState
  exit: { success: boolean; code: number | null; signal: number | null } | null
}

export function launchGodot(executable: string, project: string): Promise<GodotLaunch> {
  return invoke<GodotLaunch>('launch_godot', { executable, project })
}

export function connectGodot(sessionId: string): Promise<GodotProject> {
  return invoke<GodotProject>('connect_godot', { sessionId })
}

export function queryGodotProject(sessionId: string): Promise<GodotProject> {
  return invoke<GodotProject>('query_godot_project', { sessionId })
}

export function queryGodotScene(sessionId: string): Promise<GodotScene> {
  return invoke<GodotScene>('query_godot_scene', { sessionId })
}

export function statusGodotSession(sessionId: string): Promise<GodotStatus> {
  return invoke<GodotStatus>('status_godot_session', { sessionId })
}

export function releaseGodotSession(sessionId: string): Promise<void> {
  return invoke<void>('release_godot_session', { sessionId })
}
