import { invoke } from '@tauri-apps/api/core'

export type CapabilityId =
  | 'engines'
  | 'agent'
  | 'libraries'
  | 'projectRegistration'
  | 'chatHistory'
  | 'legacyHistory'
  | 'modelSettings'
  | 'settings'

export interface HostStatus {
  backend: 'rust'
  processId: number
  os: string
  architecture: string
  version: string
  migration: 'in_progress'
  /** Compile-time modules, not editor connection status or complete engine support. */
  engineModules: { unity: boolean; unreal: boolean; godot: boolean }
  capabilities: readonly { id: CapabilityId; available: boolean }[]
}

export function getHostStatus(): Promise<HostStatus> {
  return invoke<HostStatus>('host_status')
}
