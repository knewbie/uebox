<script setup lang="ts">
import { invoke } from '@tauri-apps/api/core'
import { onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import AppAlert from '@renderer/components/AppAlert.vue'
import AppButton from '@renderer/components/AppButton.vue'

interface Endpoint {
  id: string
  title: string
  url: string
  authenticated: boolean
}

interface Change {
  endpoints: Endpoint[]
  credentialCleanupPending: boolean
}

defineProps<{ inspecting: boolean }>()
const emit = defineEmits<{ inspect: [id: string] }>()
const { t } = useI18n()
const title = ref('')
const endpoint = ref('')
const bearer = ref('')
const entries = ref<Endpoint[]>([])
const pendingRemoval = ref<Endpoint | null>(null)
const busy = ref(false)
const error = ref<string | null>(null)
const cleanup = ref(false)
const errors = new Set([
  'mcp_busy',
  'mcp_invalid_endpoint',
  'mcp_catalog_invalid',
  'mcp_endpoint_duplicate',
  'credential_unavailable',
  'credential_access_denied',
  'credential_invalid_secret'
])

async function operation(work: () => Promise<void>): Promise<void> {
  if (busy.value) return
  busy.value = true
  error.value = null
  cleanup.value = false
  try {
    await work()
  } catch (cause) {
    error.value = typeof cause === 'string' && errors.has(cause) ? cause : 'mcp_catalog_unavailable'
  } finally {
    busy.value = false
  }
}

function refresh(): Promise<void> {
  return operation(async () => {
    entries.value = await invoke<Endpoint[]>('list_mcp_endpoints')
  })
}

function update(change: Change): void {
  entries.value = change.endpoints
  cleanup.value = change.credentialCleanupPending
}

function save(): Promise<void> {
  return operation(async () => {
    const token = bearer.value || null
    bearer.value = ''
    update(
      await invoke<Change>('save_mcp_endpoint', {
        title: title.value.trim(),
        endpoint: endpoint.value.trim(),
        bearer: token
      })
    )
    title.value = ''
    endpoint.value = ''
  })
}

function remove(): Promise<void> {
  const selected = pendingRemoval.value
  if (!selected) return Promise.resolve()
  return operation(async () => {
    update(await invoke<Change>('remove_mcp_endpoint', { endpointId: selected.id }))
    pendingRemoval.value = null
  })
}

onMounted(refresh)
</script>

<template>
  <div class="mcp-saved">
    <form class="saved-form" @submit.prevent="save">
      <label for="mcp-save-title">{{ t('tauriPreview.mcp.saved.name') }}</label>
      <input
        id="mcp-save-title"
        v-model="title"
        maxlength="256"
        :disabled="busy || inspecting"
        required
      />
      <label for="mcp-save-url">{{ t('tauriPreview.mcp.endpoint') }}</label>
      <input
        id="mcp-save-url"
        v-model="endpoint"
        type="url"
        maxlength="2048"
        :disabled="busy || inspecting"
        required
      />
      <label for="mcp-save-bearer">{{ t('tauriPreview.mcp.saved.bearer') }}</label>
      <input
        id="mcp-save-bearer"
        v-model="bearer"
        type="password"
        autocomplete="off"
        maxlength="2560"
        :disabled="busy || inspecting"
      />
      <AppButton
        html-type="submit"
        :disabled="busy || inspecting || !title.trim() || !endpoint.trim()"
      >
        {{ t('tauriPreview.mcp.saved.save') }}
      </AppButton>
    </form>
    <AppAlert v-if="error" type="error" :message="t(`tauriPreview.mcp.errors.${error}`)" />
    <AppAlert v-if="cleanup" type="warning" :message="t('tauriPreview.mcp.saved.cleanup')" />
    <ul v-if="entries.length">
      <li v-for="entry in entries" :key="entry.id" class="saved-item">
        <strong>{{ entry.title }}</strong>
        <code>{{ entry.url }}</code>
        <span v-if="entry.authenticated">{{ t('tauriPreview.mcp.saved.authorized') }}</span>
        <div class="saved-actions">
          <AppButton :disabled="busy || inspecting" @click="emit('inspect', entry.id)">
            {{ t('tauriPreview.mcp.inspect') }}
          </AppButton>
          <AppButton :disabled="busy || inspecting" @click="pendingRemoval = entry">
            {{ t('tauriPreview.mcp.saved.remove') }}
          </AppButton>
        </div>
      </li>
    </ul>
    <p v-else-if="!busy && !error">{{ t('tauriPreview.mcp.saved.empty') }}</p>
    <div v-if="pendingRemoval">
      <p>{{ t('tauriPreview.mcp.saved.confirm', { name: pendingRemoval.title }) }}</p>
      <div class="saved-actions">
        <AppButton :disabled="busy || inspecting" @click="remove">
          {{ t('tauriPreview.mcp.saved.remove') }}
        </AppButton>
        <AppButton :disabled="busy" @click="pendingRemoval = null">
          {{ t('tauriPreview.mcp.saved.cancel') }}
        </AppButton>
      </div>
    </div>
    <AppButton :disabled="busy || inspecting" @click="refresh">
      {{ t('tauriPreview.mcp.saved.refresh') }}
    </AppButton>
  </div>
</template>

<style scoped>
.mcp-saved {
  margin-block: var(--space-4);
}

.saved-form,
.saved-item {
  display: grid;
  justify-items: start;
  gap: var(--space-2);
  margin-block-end: var(--space-4);
  overflow-wrap: anywhere;
}

.saved-form input {
  width: min(100%, calc(var(--space-12) * 9));
}

.saved-actions {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-2);
}
</style>
