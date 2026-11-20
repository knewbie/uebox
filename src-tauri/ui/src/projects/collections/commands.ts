import { invoke } from '@tauri-apps/api/core'
import { z } from 'zod'

const collectionSchema = z.object({
  id: z.string().min(1),
  name: z.string().nullable(),
  projectIds: z.array(z.string().min(1))
})
const collectionsSchema = z.array(collectionSchema)
export type ProjectCollection = z.infer<typeof collectionSchema>
export type CollectionDirection = 'up' | 'down'

const importSchema = z.object({
  collections: collectionsSchema,
  inserted: z.number().int().nonnegative(),
  alreadyImported: z.number().int().nonnegative()
})
export type ImportedCollections = z.infer<typeof importSchema>

export async function importProjectCollections(manifest: string): Promise<ImportedCollections> {
  try {
    return importSchema.parse(await invoke<unknown>('import_project_collections', { manifest }))
  } catch (cause) {
    if (cause === 'project_collections_import_missing_project') {
      throw new Error('importMissingProject')
    }
    throw new Error('importError')
  }
}

async function request(
  command: string,
  args?: Record<string, unknown>
): Promise<ProjectCollection[]> {
  try {
    const result = args ? await invoke<unknown>(command, args) : await invoke<unknown>(command)
    return collectionsSchema.parse(result)
  } catch {
    // Native and validation errors may contain database paths or project data.
    throw new Error('project_collections_unavailable')
  }
}

export function listProjectCollections(): Promise<ProjectCollection[]> {
  return request('list_project_collections')
}

export function createProjectCollection(name: string | null): Promise<ProjectCollection[]> {
  return request('create_project_collection', { name })
}

export function renameProjectCollection(
  collectionId: string,
  name: string | null
): Promise<ProjectCollection[]> {
  return request('rename_project_collection', { collectionId, name })
}

export function moveProjectCollection(
  collectionId: string,
  direction: CollectionDirection
): Promise<ProjectCollection[]> {
  return request('move_project_collection', { collectionId, direction })
}

export function setProjectCollectionMembership(
  collectionId: string,
  registrationId: string,
  belongs: boolean
): Promise<ProjectCollection[]> {
  return request('set_project_collection_membership', { collectionId, registrationId, belongs })
}

export function removeProjectCollection(collectionId: string): Promise<ProjectCollection[]> {
  return request('remove_project_collection', { collectionId })
}
