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

export interface ProjectImportSummary {
  inserted: number
  alreadyRegistered: number
  duplicateInFile: number
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

export function importRegisteredProjects(manifest: string): Promise<ProjectImportSummary> {
  return invoke<ProjectImportSummary>('import_registered_projects', { manifest })
}

export function openRegisteredProjectFolder(projectId: string): Promise<void> {
  return invoke('open_registered_project_folder', { projectId })
}
