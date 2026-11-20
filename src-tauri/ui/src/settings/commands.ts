import { invoke } from '@tauri-apps/api/core'

export type ModelPurpose =
  | 'chat'
  | 'agent'
  | 'vision'
  | 'summary'
  | 'embedding'
  | 'image'
  | 'video'
  | 'model3d'
  | 'realtime'
  | 'tts'
  | 'stt'
  | 'music'
  | 'sfx'
  | 'search'
  | 'judge'

type StoredAuthorization = 'none' | 'needsAuthorization' | 'keychain' | 'environment'

export interface SavedModel {
  id: string
  title: string
  endpoint: string
  model: string
  authorization: StoredAuthorization
  environment: string | null
}

export interface ModelCatalog {
  profiles: SavedModel[]
  bindings: Partial<Record<ModelPurpose, string>>
}

export type ModelAuthorizationInput =
  | { kind: 'keep' }
  | { kind: 'none' }
  | { kind: 'keychain'; bearer: string }
  | { kind: 'environment'; name: string }

export interface ModelProfileInput {
  id: string | null
  title: string
  endpoint: string
  model: string
  authorization: ModelAuthorizationInput
}

interface ModelUpdateResult {
  catalog: ModelCatalog
  credentialCleanupPending: boolean
}

interface ModelImportResult {
  catalog: ModelCatalog
  inserted: number
  alreadyImported: number
  needsAuthorization: number
}

export function getModelSettings(): Promise<ModelCatalog> {
  return invoke('get_model_settings')
}

export function saveModelProfile(profile: ModelProfileInput): Promise<ModelUpdateResult> {
  return invoke('save_model_profile', { profile })
}

export function removeModelProfile(profileId: string): Promise<ModelUpdateResult> {
  return invoke('remove_model_profile', { profileId })
}

export function bindModelProfile(
  purpose: ModelPurpose,
  profileId: string | null
): Promise<ModelCatalog> {
  return invoke('bind_model_profile', { purpose, profileId })
}

export function importModelProfiles(manifest: string): Promise<ModelImportResult> {
  return invoke('import_model_profiles', { manifest })
}
