<script setup lang="ts">
import { invoke } from '@tauri-apps/api/core'
import { ref } from 'vue'
import { useI18n } from 'vue-i18n'
import AppAlert from '@renderer/components/AppAlert.vue'
import AppButton from '@renderer/components/AppButton.vue'

interface Inspection {
  protocolVersion: string
  totalTools: number
  toolNames: string[]
  sessionCleanupPending: boolean
}

const { t } = useI18n()
const endpoint = ref('')
const inspecting = ref(false)
const inspection = ref<Inspection | null>(null)
const error = ref<string | null>(null)
const errors = new Set([
  'mcp_invalid_endpoint',
  'mcp_timeout',
  'mcp_redirect',
  'mcp_authorization_required',
  'mcp_version',
  'mcp_no_tools'
])

async function inspect(): Promise<void> {
  const selected = endpoint.value.trim()
  if (inspecting.value || !selected) return
  inspecting.value = true
  inspection.value = null
  error.value = null
  try {
    inspection.value = await invoke<Inspection>('inspect_mcp_endpoint', { endpoint: selected })
  } catch (cause) {
    error.value = typeof cause === 'string' && errors.has(cause) ? cause : 'mcp_unavailable'
  } finally {
    inspecting.value = false
  }
}
</script>

<template>
  <section id="mcp" class="panel" aria-labelledby="mcp-heading">
    <h2 id="mcp-heading">{{ t('tauriPreview.mcp.title') }}</h2>
    <p class="section-description">{{ t('tauriPreview.mcp.intro') }}</p>
    <form class="mcp-form" @submit.prevent="inspect">
      <label for="mcp-endpoint">{{ t('tauriPreview.mcp.endpoint') }}</label>
      <input
        id="mcp-endpoint"
        v-model="endpoint"
        type="url"
        maxlength="2048"
        required
        :disabled="inspecting"
      />
      <AppButton html-type="submit" :disabled="inspecting || !endpoint.trim()">
        {{ t(inspecting ? 'tauriPreview.mcp.inspecting' : 'tauriPreview.mcp.inspect') }}
      </AppButton>
    </form>
    <AppAlert v-if="error" type="error" :message="t(`tauriPreview.mcp.errors.${error}`)" />
    <template v-if="inspection">
      <p role="status">
        {{
          t('tauriPreview.mcp.result', {
            version: inspection.protocolVersion,
            count: inspection.totalTools,
            shown: inspection.toolNames.length
          })
        }}
      </p>
      <ul v-if="inspection.toolNames.length">
        <li v-for="(name, index) in inspection.toolNames" :key="index">
          <code>{{ name }}</code>
        </li>
      </ul>
      <AppAlert
        v-if="inspection.sessionCleanupPending"
        type="warning"
        :message="t('tauriPreview.mcp.cleanup')"
      />
    </template>
  </section>
</template>

<style scoped src="../panel.css"></style>

<style scoped>
.mcp-form {
  display: grid;
  justify-items: start;
  gap: var(--space-2);
  margin-block-end: var(--space-4);
}

.mcp-form input {
  width: min(100%, calc(var(--space-12) * 9));
}
</style>
