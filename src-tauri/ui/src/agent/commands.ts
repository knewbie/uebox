import { invoke } from '@tauri-apps/api/core'

export type AgentEngine = 'godot' | 'unity' | 'unreal'
export type AgentTarget =
  | { sessionId: string; allowGameObjectRename?: true }
  | { connectionId: string; projectRoot: string; allowActorLabelChanges?: true }
export type AgentPhase =
  | 'running'
  | 'awaiting_approval'
  | 'cancelling'
  | 'draining'
  | 'completed'
  | 'cancelled'
  | 'failed'
export interface AgentRunUpdate {
  agentId: string
  runId: number
  phase: AgentPhase
  fragments: string[]
  error: string | null
}
export function createAgent(
  engine: AgentEngine,
  target: AgentTarget,
  endpoint: string,
  model: string,
  credential: string | null
): Promise<{ agentId: string }> {
  return invoke(`create_${engine}_agent`, { ...target, endpoint, model, credential })
}
export function createAgentFromProfile(
  engine: AgentEngine,
  target: AgentTarget,
  profileId: string | null
): Promise<{ agentId: string }> {
  return invoke(`create_${engine}_agent_from_profile`, { ...target, profileId })
}
export function startAgent(
  engine: AgentEngine,
  agentId: string,
  text: string,
  saveHistory: boolean
): Promise<{ agentId: string; runId: number }> {
  return invoke(`start_${engine}_agent`, { agentId, text, ...(saveHistory ? { saveHistory } : {}) })
}
export function pollAgent(
  engine: AgentEngine,
  agentId: string,
  runId: number
): Promise<AgentRunUpdate> {
  return invoke(`poll_${engine}_agent`, { agentId, runId })
}
export function cancelAgent(engine: AgentEngine, agentId: string, runId: number): Promise<void> {
  return invoke(`cancel_${engine}_agent`, { agentId, runId })
}
export function closeAgent(engine: AgentEngine, agentId: string): Promise<void> {
  return invoke(`close_${engine}_agent`, { agentId })
}
