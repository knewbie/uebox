import { invoke } from '@tauri-apps/api/core'

export type ProjectEngine = 'unreal' | 'unity' | 'godot'

interface ProjectDetails {
  engine: ProjectEngine
  name: string
  path: string
}

export interface RegisteredProject extends ProjectDetails {
  id: string
}

export function listRegisteredProjects(): Promise<RegisteredProject[]> {
  return invoke<RegisteredProject[]>('list_registered_projects')
}

export function registerProject(
  engine: ProjectEngine,
  name: string,
  path: string
): Promise<RegisteredProject> {
  return invoke<RegisteredProject>('register_project', { engine, name, path })
}

export function removeRegisteredProject(id: string): Promise<boolean> {
  return invoke<boolean>('remove_registered_project', { id })
}
