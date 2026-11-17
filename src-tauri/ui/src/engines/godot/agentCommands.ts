import { invoke } from '@tauri-apps/api/core'

export type AgentPhase =
  | 'running'
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
  sessionId: string,
  endpoint: string,
  model: string,
  credential: string | null
): Promise<{ agentId: string }> {
  return invoke('create_godot_agent', { sessionId, endpoint, model, credential })
}
export function startAgent(
  agentId: string,
  text: string
): Promise<{ agentId: string; runId: number }> {
  return invoke('start_godot_agent', { agentId, text })
}
export function pollAgent(agentId: string, runId: number): Promise<AgentRunUpdate> {
  return invoke('poll_godot_agent', { agentId, runId })
}
export function cancelAgent(agentId: string, runId: number): Promise<void> {
  return invoke('cancel_godot_agent', { agentId, runId })
}
export function closeAgent(agentId: string): Promise<void> {
  return invoke('close_godot_agent', { agentId })
}
