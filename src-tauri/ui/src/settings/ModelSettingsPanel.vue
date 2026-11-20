<script setup lang="ts">
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import AppAlert from '@renderer/components/AppAlert.vue'
import AppButton from '@renderer/components/AppButton.vue'
import {
  bindModelProfile,
  getModelSettings,
  importModelProfiles,
  removeModelProfile,
  saveModelProfile,
  type ModelAuthorizationInput,
  type ModelCatalog,
  type ModelPurpose,
  type SavedModel
} from './commands'

const { t } = useI18n()
const catalog = ref<ModelCatalog>({ profiles: [], bindings: {} })
const open = ref(false)
const busy = ref(false)
const error = ref<string | null>(null)
const cleanupPending = ref(false)
const report = ref<{
  inserted: number
  alreadyImported: number
  needsAuthorization: number
} | null>(null)
const selectedFile = ref<File | null>(null)
const fileInput = ref<HTMLInputElement | null>(null)
const editing = ref<string | null>(null)
const title = ref('')
const endpoint = ref('')
const model = ref('')
const auth = ref<'keep' | 'none' | 'keychain' | 'environment'>('none')
const bearer = ref('')
const environment = ref('')
const purpose = ref<ModelPurpose>('agent')
const pendingRemoval = ref<SavedModel | null>(null)
const purposes: readonly ModelPurpose[] = [
  'chat',
  'agent',
  'vision',
  'summary',
  'embedding',
  'image',
  'video',
  'model3d',
  'realtime',
  'tts',
  'stt',
  'music',
  'sfx',
  'search',
  'judge'
]
const allowedErrors = new Set([
  'credential_reauthorization_required',
  'credential_invalid_secret',
  'credential_unavailable',
  'credential_access_denied',
  'settings_invalid',
  'settings_locked',
  'model_profile_import_invalid',
  'model_profile_import_limit',
  'model_profile_import_version'
])
const binding = computed(() => catalog.value.bindings[purpose.value] ?? '')

function setCatalog(next: ModelCatalog): void {
  catalog.value = next
}

async function apply(operation: () => Promise<void>): Promise<void> {
  if (busy.value) return
  busy.value = true
  error.value = null
  cleanupPending.value = false
  try {
    await operation()
  } catch (cause) {
    const code = cause instanceof Error ? cause.message : cause
    error.value = typeof code === 'string' && allowedErrors.has(code) ? code : 'generic'
  } finally {
    busy.value = false
  }
}

function resetEditor(profile?: SavedModel): void {
  editing.value = profile?.id ?? null
  title.value = profile?.title ?? ''
  endpoint.value = profile?.endpoint ?? ''
  model.value = profile?.model ?? ''
  auth.value = profile ? 'keep' : 'none'
  environment.value = profile?.environment ?? ''
  bearer.value = ''
}

async function toggle(): Promise<void> {
  if (busy.value) return
  open.value = !open.value
  if (open.value) await apply(async () => setCatalog(await getModelSettings()))
  else resetEditor()
}

function credentialUpdate(): ModelAuthorizationInput {
  switch (auth.value) {
    case 'keychain':
      return { kind: 'keychain', bearer: bearer.value }
    case 'environment':
      return { kind: 'environment', name: environment.value.trim() }
    case 'keep':
      return { kind: 'keep' }
    case 'none':
      return { kind: 'none' }
  }
}

async function save(): Promise<void> {
  await apply(async () => {
    const authorization = credentialUpdate()
    bearer.value = ''
    const result = await saveModelProfile({
      id: editing.value,
      title: title.value.trim(),
      endpoint: endpoint.value.trim(),
      model: model.value.trim(),
      authorization
    })
    setCatalog(result.catalog)
    cleanupPending.value = result.credentialCleanupPending
    resetEditor()
  })
}

async function remove(): Promise<void> {
  const profile = pendingRemoval.value
  if (!profile) return
  await apply(async () => {
    const result = await removeModelProfile(profile.id)
    setCatalog(result.catalog)
    cleanupPending.value = result.credentialCleanupPending
    if (editing.value === profile.id) resetEditor()
    pendingRemoval.value = null
  })
}

async function changeBinding(event: Event): Promise<void> {
  const id = (event.target as HTMLSelectElement).value || null
  await apply(async () => setCatalog(await bindModelProfile(purpose.value, id)))
}

function chooseFile(event: Event): void {
  selectedFile.value = (event.target as HTMLInputElement).files?.[0] ?? null
  report.value = null
  error.value = null
}

async function importSelected(): Promise<void> {
  const file = selectedFile.value
  if (!file) return
  await apply(async () => {
    if (file.size > 512 * 1024) throw new Error('model_profile_import_limit')
    let manifest: string
    try {
      manifest = new TextDecoder('utf-8', { fatal: true }).decode(await file.arrayBuffer())
    } catch {
      throw new Error('model_profile_import_invalid')
    }
    const result = await importModelProfiles(manifest)
    setCatalog(result.catalog)
    report.value = result
    selectedFile.value = null
    if (fileInput.value) fileInput.value.value = ''
  })
}
</script>

<template>
  <section id="model-settings" class="panel" aria-labelledby="model-settings-heading">
    <h2 id="model-settings-heading">{{ t('tauriPreview.modelSettings.title') }}</h2>
    <AppButton :disabled="busy" :aria-expanded="open" @click="toggle">
      {{ t(open ? 'tauriPreview.modelSettings.close' : 'tauriPreview.modelSettings.open') }}
    </AppButton>
    <template v-if="open">
      <AppAlert
        v-if="error"
        type="error"
        show-icon
        :message="t(`tauriPreview.modelSettings.errors.${error}`)"
      />
      <AppAlert
        v-if="cleanupPending"
        type="warning"
        show-icon
        :message="t('tauriPreview.modelSettings.cleanupPending')"
      />
      <ul v-if="catalog.profiles.length" class="model-settings-list">
        <li v-for="profile in catalog.profiles" :key="profile.id">
          <div>
            <strong>{{ profile.title }}</strong>
            <p>{{ profile.model }}</p>
            <p v-if="catalog.bindings.agent === profile.id">
              {{ t('tauriPreview.modelSettings.cliModelId') }}
              <code class="model-profile-id">{{ profile.id }}</code>
            </p>
            <p v-if="profile.authorization === 'needsAuthorization'">
              {{ t('tauriPreview.modelSettings.needsAuthorization') }}
            </p>
          </div>
          <div class="model-settings-actions">
            <AppButton :disabled="busy" @click="resetEditor(profile)">{{
              t('tauriPreview.modelSettings.edit')
            }}</AppButton>
            <AppButton :disabled="busy" @click="pendingRemoval = profile">{{
              t('tauriPreview.modelSettings.remove')
            }}</AppButton>
          </div>
        </li>
      </ul>
      <p v-else>{{ t('tauriPreview.modelSettings.empty') }}</p>
      <div v-if="pendingRemoval" class="model-settings-confirm">
        <p>{{ t('tauriPreview.modelSettings.confirmRemove', { title: pendingRemoval.title }) }}</p>
        <AppButton :disabled="busy" @click="remove">{{
          t('tauriPreview.modelSettings.remove')
        }}</AppButton>
        <AppButton :disabled="busy" @click="pendingRemoval = null">{{
          t('tauriPreview.modelSettings.cancel')
        }}</AppButton>
      </div>

      <form class="model-settings-fields" @submit.prevent="save">
        <label
          >{{ t('tauriPreview.modelSettings.name') }}
          <input v-model="title" required maxlength="256" :disabled="busy"
        /></label>
        <label
          >{{ t('tauriPreview.modelSettings.endpoint') }}
          <input v-model="endpoint" type="url" required maxlength="2048" :disabled="busy"
        /></label>
        <label
          >{{ t('tauriPreview.modelSettings.model') }}
          <input v-model="model" required maxlength="256" :disabled="busy"
        /></label>
        <label>
          {{ t('tauriPreview.modelSettings.authorization') }}
          <select v-model="auth" :disabled="busy">
            <option v-if="editing" value="keep">
              {{ t('tauriPreview.modelSettings.auth.keep') }}
            </option>
            <option value="none">{{ t('tauriPreview.modelSettings.auth.none') }}</option>
            <option value="keychain">{{ t('tauriPreview.modelSettings.auth.keychain') }}</option>
            <option value="environment">
              {{ t('tauriPreview.modelSettings.auth.environment') }}
            </option>
          </select>
        </label>
        <label v-if="auth === 'keychain'">
          {{ t('tauriPreview.modelSettings.bearer') }}
          <input v-model="bearer" type="password" autocomplete="off" required :disabled="busy" />
        </label>
        <label v-if="auth === 'environment'">
          {{ t('tauriPreview.modelSettings.environment') }}
          <input v-model="environment" autocomplete="off" required :disabled="busy" />
        </label>
        <div class="model-settings-actions">
          <AppButton html-type="submit" variant="primary" :disabled="busy">{{
            t('tauriPreview.modelSettings.save')
          }}</AppButton>
          <AppButton v-if="editing" :disabled="busy" @click="resetEditor()">{{
            t('tauriPreview.modelSettings.cancel')
          }}</AppButton>
        </div>
      </form>

      <div v-if="catalog.profiles.length" class="model-settings-fields">
        <label>
          {{ t('tauriPreview.modelSettings.purpose') }}
          <select v-model="purpose" :disabled="busy">
            <option v-for="item in purposes" :key="item" :value="item">
              {{ t(`aiProvider.roles.${item}`) }}
            </option>
          </select>
        </label>
        <label>
          {{ t('tauriPreview.modelSettings.binding') }}
          <select :value="binding" :disabled="busy" @change="changeBinding">
            <option value="">{{ t('tauriPreview.modelSettings.unassigned') }}</option>
            <option v-for="profile in catalog.profiles" :key="profile.id" :value="profile.id">
              {{ profile.title }}
            </option>
          </select>
        </label>
      </div>

      <div class="model-settings-fields">
        <label>
          {{ t('tauriPreview.modelSettings.importFile') }}
          <input
            ref="fileInput"
            type="file"
            accept=".json,application/json"
            :disabled="busy"
            @change="chooseFile"
          />
        </label>
        <AppButton :disabled="busy || !selectedFile" @click="importSelected">{{
          t('tauriPreview.modelSettings.import')
        }}</AppButton>
        <p v-if="report" role="status">{{ t('tauriPreview.modelSettings.imported', report) }}</p>
      </div>
    </template>
  </section>
</template>

<style scoped>
.model-settings-fields,
.model-settings-list {
  display: grid;
  gap: var(--space-3);
  margin-block: var(--space-4);
}
.model-settings-fields label {
  display: grid;
  gap: var(--space-2);
}
.model-settings-fields input,
.model-settings-fields select {
  color: var(--color-text-primary);
  background-color: var(--color-bg-page);
  border: 1px solid var(--color-border);
  border-radius: var(--radius-sm);
  padding: var(--space-2);
}
.model-settings-list {
  list-style: none;
  padding: 0;
}
.model-settings-list li {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: var(--space-3);
}
.model-settings-actions {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-2);
}
.model-profile-id {
  user-select: all;
  overflow-wrap: anywhere;
}
</style>
