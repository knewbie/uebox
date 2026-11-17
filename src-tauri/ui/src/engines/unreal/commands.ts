import { invoke } from '@tauri-apps/api/core'

export interface UnrealListenerInfo {
  address: string
  port: number
}

export interface UnrealInstance {
  connection_id: string
  project_name: string
  project_path: string
  interactive: boolean
}

export function startUnrealListener(port: number): Promise<UnrealListenerInfo> {
  return invoke<UnrealListenerInfo>('start_unreal_listener', { port })
}

export function listUnrealInstances(): Promise<UnrealInstance[]> {
  return invoke<UnrealInstance[]>('list_unreal_instances')
}

export function queryUnrealProject(connectionId: string): Promise<Record<string, unknown>> {
  return invoke<Record<string, unknown>>('query_unreal_project', { connectionId })
}

export function stopUnrealListener(): Promise<void> {
  return invoke<void>('stop_unreal_listener')
}
