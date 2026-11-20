/** @vitest-environment node */
import { expect, it } from 'vitest'
import { exportModelProfiles } from './modelProfileExport'

const chat = (overrides: Record<string, unknown> = {}): Record<string, unknown> => ({
  id: 'gateway',
  displayName: 'Fixture Gateway',
  kind: 'chat',
  protocol: 'openai-completions',
  baseUrl: 'https://fixture.invalid/team/v1/',
  apiKey: { kind: 'none' },
  models: [{ id: 'host/model', displayName: '中文模型' }],
  ...overrides
})

const bind = (
  providerId: string,
  modelId = 'host/model'
): { providerId: string; modelId: string } => ({
  providerId,
  modelId
})
const zero = { providers: 0, models: 0, bindings: 0 }

it('converts the exact legacy protocol and keeps explicit Rust-compatible purpose bindings', () => {
  const settings = {
    version: 3,
    providers: [chat({ models: [{ id: 'host/model', displayName: '中文模型' }, { id: 'other' }] })],
    roles: { chat: bind('gateway'), agent: bind('gateway', 'other'), fast: bind('gateway') }
  }
  const result = exportModelProfiles(JSON.stringify(settings))
  expect(result).toEqual({
    success: true,
    config: {
      format: 'uebox-model-profiles',
      version: 1,
      profiles: [
        {
          providerId: 'gateway',
          providerName: 'Fixture Gateway',
          model: 'host/model',
          title: '中文模型',
          endpoint: 'https://fixture.invalid/team/v1/chat/completions',
          protocol: 'openai-chat-completions',
          requiresReauthorization: false
        },
        {
          providerId: 'gateway',
          providerName: 'Fixture Gateway',
          model: 'other',
          title: 'other',
          endpoint: 'https://fixture.invalid/team/v1/chat/completions',
          protocol: 'openai-chat-completions',
          requiresReauthorization: false
        }
      ],
      bindings: {
        chat: { providerId: 'gateway', model: 'host/model' },
        agent: { providerId: 'gateway', model: 'other' },
        summary: { providerId: 'gateway', model: 'host/model' }
      }
    },
    counts: { unsupported: zero, invalid: zero }
  })
  expect(exportModelProfiles(settings)).toEqual(result)
})

it('only inspects the credential discriminant and never invokes credential or header getters', () => {
  const forbidden = (): never => {
    throw new Error('must never read credential payload')
  }
  const credential = { kind: 'oauth' }
  for (const name of ['id', 'provider', 'accessToken', 'refreshToken', 'payload']) {
    Object.defineProperty(credential, name, { enumerable: true, get: forbidden })
  }
  const provider = chat({ apiKey: credential })
  Object.defineProperty(provider, 'headers', { enumerable: true, get: forbidden })
  const result = exportModelProfiles({ providers: [provider] })
  expect(result).toMatchObject({
    success: true,
    config: { profiles: [{ requiresReauthorization: true }] }
  })
  expect(JSON.stringify(result)).not.toMatch(
    /credential payload|apiKey|headers|accessToken|refreshToken/
  )
})

it('requires fresh authorization for literal, shell, env, OAuth and ambiguous settings', () => {
  const auth = [
    { kind: 'literal', id: 'hidden-encrypted-reference' },
    { kind: 'shell', command: 'hidden-command' },
    { kind: 'env', name: 'HIDDEN_ENV_NAME' },
    { kind: 'oauth', provider: 'hidden-account', accessToken: 'hidden-access-token' },
    { kind: 'none' },
    null,
    'hidden-plaintext'
  ]
  const result = exportModelProfiles(
    JSON.stringify({
      version: 3,
      providers: auth.map((apiKey, index) =>
        chat({
          id: `gateway-${index}`,
          apiKey,
          ...(index === 4 ? { headers: { Authorization: 'hidden-header' } } : {}),
          accountFile: 'hidden-file',
          models: [{ id: 'host/model', headers: { 'X-Fixture-Key': 'hidden-model-header' } }]
        })
      ),
      oauthPayload: { refreshToken: 'hidden-refresh' }
    })
  )
  expect(result.success).toBe(true)
  if (!result.success) return
  expect(result.config.profiles).toHaveLength(auth.length)
  expect(result.config.profiles.every((profile) => profile.requiresReauthorization)).toBe(true)
  expect(JSON.stringify(result)).not.toMatch(/hidden|apiKey|headers|accountFile|oauthPayload/)
})

it('counts unsupported transports and non-chat usages separately from invalid rows and bindings', () => {
  const result = exportModelProfiles({
    version: 2,
    providers: [
      chat({ models: [{ id: 'host/model' }, { id: 'host/model' }, { displayName: 'Missing ID' }] }),
      chat({ id: 'responses', protocol: 'openai-responses' }),
      chat({ id: 'anthropic', protocol: 'anthropic-messages' }),
      chat({ id: 'voice', kind: 'tts' }),
      chat({
        id: 'legacy-vision',
        kind: undefined,
        models: [{ id: 'host/model', supportsVision: true }]
      }),
      chat({
        id: 'legacy-embedding',
        kind: undefined,
        models: [{ id: 'embed', supportsEmbedding: true }]
      }),
      chat({ id: 'invalid-endpoint', baseUrl: 'https://user:secret@fixture.invalid/v1' })
    ],
    roles: {
      chat: bind('gateway'),
      agent: bind('responses'),
      vision: bind('legacy-vision'),
      summary: bind('missing'),
      tts: bind('voice'),
      embedding: bind('legacy-embedding', 'embed'),
      judge: bind('gateway'),
      extra: bind('gateway')
    }
  })
  expect(result).toMatchObject({
    success: true,
    config: {
      profiles: [{ providerId: 'gateway' }, { providerId: 'legacy-vision' }],
      bindings: { chat: { providerId: 'gateway' }, vision: { providerId: 'legacy-vision' } }
    },
    counts: {
      unsupported: { providers: 4, models: 4, bindings: 5 },
      invalid: { providers: 1, models: 3, bindings: 1 }
    }
  })
  expect(JSON.stringify(result)).not.toContain('secret')
})

it('rejects credential-bearing URLs and preserves HTTP and existing completion paths without requests', () => {
  const urls = [
    'http://127.0.0.1:9090/v1/chat/completions///',
    'https://fixture.invalid/v1?api_key=hidden-query',
    'https://fixture.invalid/v1#hidden-fragment',
    'https://@fixture.invalid/v1',
    'file:///tmp/hidden-path',
    'https://fixture.invalid\\hidden',
    'https://fixture.invalid/v1?'
  ]
  const result = exportModelProfiles({
    version: 1,
    providers: urls.map((baseUrl, index) => chat({ id: `gateway-${index}`, baseUrl }))
  })
  expect(result).toMatchObject({
    success: true,
    config: { profiles: [{ endpoint: 'http://127.0.0.1:9090/v1/chat/completions' }] },
    counts: { unsupported: zero, invalid: { providers: 6, models: 6, bindings: 0 } }
  })
  expect(JSON.stringify(result)).not.toContain('hidden')
})

it('rejects malformed envelopes and unknown versions using fixed errors', () => {
  for (const malformed of ['{"hidden":', [], { roles: {} }, { providers: [], roles: [] }]) {
    expect(exportModelProfiles(malformed)).toEqual({
      success: false,
      code: 'model_profile_export_invalid'
    })
  }
  expect(exportModelProfiles({ version: 4, providers: [] })).toEqual({
    success: false,
    code: 'model_profile_export_unsupported_version'
  })
  const models = Array.from({ length: 129 }, (_, index) => ({ id: `model-${index}` }))
  expect(exportModelProfiles({ version: 3, providers: [chat({ models })] })).toEqual({
    success: false,
    code: 'model_profile_export_too_large'
  })
})

it('keeps the first provider and gives an explicit summary precedence over the old fast alias', () => {
  const result = exportModelProfiles({
    version: 3,
    providers: [
      chat({ models: [{ id: 'host/model' }, { id: 'summary-choice' }] }),
      chat({ baseUrl: 'https://different.invalid/replacement' })
    ],
    roles: { fast: bind('gateway'), summary: bind('gateway', 'summary-choice') }
  })
  expect(result).toMatchObject({
    success: true,
    config: { bindings: { summary: { providerId: 'gateway', model: 'summary-choice' } } },
    counts: { unsupported: zero, invalid: { providers: 1, models: 1, bindings: 0 } }
  })
  expect(JSON.stringify(result)).not.toContain('different.invalid')
})
