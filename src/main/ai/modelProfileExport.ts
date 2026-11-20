import { Buffer } from 'node:buffer'
import {
  isDoubaoRealtimeBaseUrl,
  isOpenAiRealtimeModelId,
  model3dApiFromBaseUrl
} from '../../shared/aiProvider'

const MAX_INPUT_BYTES = 2 * 1024 * 1024
const MAX_INPUT_ROWS = 4096
const MAX_PROFILES = 128
const MAX_CONFIG_BYTES = 512 * 1024

export type PortableModelPurpose = 'chat' | 'agent' | 'vision' | 'summary'

export interface PortableModelSelection {
  providerId: string
  model: string
}

export interface PortableModelProfile extends PortableModelSelection {
  providerName: string
  title: string
  endpoint: string
  protocol: 'openai-chat-completions'
  requiresReauthorization: boolean
}

export interface PortableModelConfig {
  format: 'uebox-model-profiles'
  version: 1
  profiles: PortableModelProfile[]
  bindings: Partial<Record<PortableModelPurpose, PortableModelSelection>>
}

interface RowCounts {
  providers: number
  models: number
  bindings: number
}

export interface ModelProfileExportCounts {
  unsupported: RowCounts
  invalid: RowCounts
}

export type ModelProfileExportResult =
  | { success: true; config: PortableModelConfig; counts: ModelProfileExportCounts }
  | {
      success: false
      code:
        | 'model_profile_export_invalid'
        | 'model_profile_export_unsupported_version'
        | 'model_profile_export_too_large'
    }

const control = /\p{Cc}/u
const completionPath = '/chat/completions'
const tooLarge = { success: false, code: 'model_profile_export_too_large' } as const
const invalid = { success: false, code: 'model_profile_export_invalid' } as const

function record(value: unknown): object | undefined {
  return typeof value === 'object' && value !== null && !Array.isArray(value) ? value : undefined
}

/** Read selected own data properties without invoking getters or spreading input objects. */
function own(value: object | undefined, key: PropertyKey): unknown {
  return value ? Object.getOwnPropertyDescriptor(value, key)?.value : undefined
}

function text(value: unknown, limit = 256): string | undefined {
  if (typeof value !== 'string' || control.test(value)) return undefined
  const trimmed = value.trim()
  return trimmed && Buffer.byteLength(trimmed) <= limit ? trimmed : undefined
}

function completionEndpoint(baseUrl: string): string | undefined {
  const authority = /^https?:\/\/([^/?#]+)/i.exec(baseUrl)?.[1]
  if (!authority || authority.includes('@') || /[?#\\]/.test(baseUrl)) return undefined
  try {
    const url = new URL(baseUrl)
    if (url.username || url.password || !url.hostname) return undefined
    const path = url.pathname.replace(/\/+$/, '')
    url.pathname = path.endsWith(completionPath) ? path : `${path}${completionPath}`
    const endpoint = url.href
    return Buffer.byteLength(endpoint) <= 2048 ? endpoint : undefined
  } catch {
    return undefined
  }
}

function legacyChat(provider: object, baseUrl: string, models: unknown[]): boolean {
  // Pre-v3 models carry the modality on each model. Match the old store's exclusions without
  // loading its provider catalog or runtime. Vision is part of Chat Completions.
  if (isDoubaoRealtimeBaseUrl(baseUrl) || model3dApiFromBaseUrl(baseUrl)) return false
  const videoApi = own(provider, 'videoApi')
  if (videoApi === 'ark-video' || videoApi === 'minimax-video' || videoApi === 'uebox-tasks')
    return false
  for (let index = 0; index < models.length; index++) {
    const model = record(own(models, index))
    if (
      own(model, 'supportsRealtimeVoice') === true ||
      own(model, 'supportsModel3d') === true ||
      own(model, 'supportsImageGeneration') === true ||
      own(model, 'supportsEmbedding') === true ||
      isOpenAiRealtimeModelId(text(own(model, 'id')) ?? '')
    )
      return false
  }
  return true
}

function isPurpose(value: string): value is PortableModelPurpose {
  return value === 'chat' || value === 'agent' || value === 'vision' || value === 'summary'
}

type ProviderState = Set<string> | 'unsupported' | 'invalid'

function mapBindings(
  roles: object,
  keys: string[],
  providers: Map<string, ProviderState>,
  counts: ModelProfileExportCounts
): PortableModelConfig['bindings'] {
  const bindings: PortableModelConfig['bindings'] = {}
  for (const key of keys) {
    if (key === 'fast' && own(roles, 'summary') != null) continue
    if (key === 'summary' && own(roles, key) == null && keys.includes('fast')) continue
    const purpose = key === 'fast' ? 'summary' : key
    if (!isPurpose(purpose)) {
      counts.unsupported.bindings++
      continue
    }
    const binding = record(own(roles, key))
    const providerId = text(own(binding, 'providerId'))
    const model = text(own(binding, 'modelId'))
    const state = providerId === undefined ? undefined : providers.get(providerId)
    if (!providerId || !model || !state || state === 'invalid') counts.invalid.bindings++
    else if (state === 'unsupported') counts.unsupported.bindings++
    else if (!state.has(model)) counts.invalid.bindings++
    else bindings[purpose] = { providerId, model }
  }
  return bindings
}

/** Pure projection of a caller-selected Electron models.json document. `config` is the wire payload. */
export function exportModelProfiles(input: unknown): ModelProfileExportResult {
  let raw = input
  if (typeof input === 'string') {
    if (Buffer.byteLength(input) > MAX_INPUT_BYTES) return tooLarge
    try {
      raw = JSON.parse(input) as unknown
    } catch {
      return invalid
    }
  }
  const source = record(raw)
  const providers = own(source, 'providers')
  const rawRoles = own(source, 'roles')
  const roles = rawRoles === undefined ? {} : record(rawRoles)
  if (!Array.isArray(providers) || !roles) return invalid
  const version = own(source, 'version')
  if (version !== undefined && version !== 1 && version !== 2 && version !== 3) {
    if (!Number.isInteger(version)) return invalid
    return { success: false, code: 'model_profile_export_unsupported_version' }
  }

  const roleKeys = Object.keys(roles)
  let rows = providers.length + roleKeys.length
  if (rows > MAX_INPUT_ROWS) return tooLarge
  const counts: ModelProfileExportCounts = {
    unsupported: { providers: 0, models: 0, bindings: 0 },
    invalid: { providers: 0, models: 0, bindings: 0 }
  }
  const profiles: PortableModelProfile[] = []
  const states = new Map<string, ProviderState>()

  for (let index = 0; index < providers.length; index++) {
    const provider = record(own(providers, index))
    const providerId = text(own(provider, 'id'))
    const protocol = text(own(provider, 'protocol'))
    const models = own(provider, 'models')
    const count = Array.isArray(models) ? models.length : 0
    rows += count
    if (rows > MAX_INPUT_ROWS) return tooLarge

    if (!provider || !providerId || !protocol || !Array.isArray(models) || states.has(providerId)) {
      counts.invalid.providers++
      counts.invalid.models += count
      continue
    }
    const baseUrl = text(own(provider, 'baseUrl'), 2048)
    const kind = own(provider, 'kind')
    if (
      protocol !== 'openai-completions' ||
      (kind !== undefined && kind !== 'chat') ||
      (kind === undefined && baseUrl && !legacyChat(provider, baseUrl, models))
    ) {
      states.set(providerId, 'unsupported')
      counts.unsupported.providers++
      counts.unsupported.models += count
      continue
    }
    const endpoint = baseUrl && completionEndpoint(baseUrl)
    if (!endpoint) {
      states.set(providerId, 'invalid')
      counts.invalid.providers++
      counts.invalid.models += count
      continue
    }

    const ids = new Set<string>()
    states.set(providerId, ids)
    const providerName = text(own(provider, 'displayName')) ?? providerId
    // Ignore every credential value and header. Presence of a header field alone makes a
    // "none" declaration ambiguous, even though header contents are never accessed.
    const requiresReauthorization =
      own(record(own(provider, 'apiKey')), 'kind') !== 'none' || Object.hasOwn(provider, 'headers')
    for (let modelIndex = 0; modelIndex < models.length; modelIndex++) {
      const entry = record(own(models, modelIndex))
      const model = text(own(entry, 'id'))
      if (!model || ids.has(model)) {
        counts.invalid.models++
        continue
      }
      if (profiles.length === MAX_PROFILES) return tooLarge
      ids.add(model)
      profiles.push({
        providerId,
        providerName,
        model,
        title: text(own(entry, 'displayName')) ?? model,
        endpoint,
        protocol: 'openai-chat-completions',
        requiresReauthorization
      })
    }
  }

  const config: PortableModelConfig = {
    format: 'uebox-model-profiles',
    version: 1,
    profiles,
    bindings: mapBindings(roles, roleKeys, states, counts)
  }
  if (Buffer.byteLength(JSON.stringify(config)) > MAX_CONFIG_BYTES) return tooLarge
  return { success: true, config, counts }
}
