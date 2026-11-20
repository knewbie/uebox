import { invoke } from '@tauri-apps/api/core'

export function attachUnityAgentTarget(
  projectRoot: string,
  port: number,
  allowClientTakeover: boolean
): Promise<{ sessionId: string }> {
  return invoke('attach_unity_agent_target', { projectRoot, port, allowClientTakeover })
}

export function releaseUnityAgentTarget(sessionId: string): Promise<void> {
  return invoke('release_unity_agent_target', { sessionId })
}
