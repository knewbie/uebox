<script setup lang="ts">
import { invoke } from '@tauri-apps/api/core'
import { onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import AppAlert from '@renderer/components/AppAlert.vue'
import AppButton from '@renderer/components/AppButton.vue'

interface Endpoint {
  endpoint: string
  allowAsk?: boolean
}

const { t } = useI18n()
const endpoint = ref<string | null>(null)
const allowAsk = ref(false)
const busy = ref(false)
const error = ref<string | null>(null)
const errors = new Set([
  'native_cli_endpoint_busy',
  'native_cli_endpoint_invalid',
  'native_cli_restart_required'
])

async function apply(action: () => Promise<Endpoint | null>): Promise<void> {
  if (busy.value) return
  busy.value = true
  error.value = null
  try {
    const selection = await action()
    endpoint.value = selection?.endpoint ?? null
    allowAsk.value = selection?.allowAsk === true
  } catch (cause) {
    error.value = typeof cause === 'string' && errors.has(cause) ? cause : 'generic'
  } finally {
    busy.value = false
  }
}

function start(): Promise<void> {
  return apply(() =>
    allowAsk.value
      ? invoke<Endpoint>('start_native_cli', { allowAsk: true })
      : invoke<Endpoint>('start_native_cli')
  )
}

function stop(): Promise<void> {
  return apply(async () => {
    await invoke('stop_native_cli')
    return null
  })
}

onMounted(() => apply(() => invoke<Endpoint | null>('native_cli_endpoint')))
</script>

<template>
  <section id="native-cli" class="panel" aria-labelledby="native-cli-heading">
    <h2 id="native-cli-heading">{{ t('tauriPreview.nativeCli.title') }}</h2>
    <p class="section-description">{{ t('tauriPreview.nativeCli.intro') }}</p>
    <AppAlert
      v-if="error"
      type="error"
      show-icon
      :message="t(`tauriPreview.nativeCli.errors.${error}`)"
    />
    <template v-if="endpoint">
      <p>{{ t('tauriPreview.nativeCli.endpoint') }}</p>
      <code class="native-cli-endpoint">{{ endpoint }}</code>
      <p v-if="allowAsk">{{ t('tauriPreview.nativeCli.askEnabled') }}</p>
      <AppButton :disabled="busy" @click="stop">{{ t('tauriPreview.nativeCli.stop') }}</AppButton>
    </template>
    <template v-else>
      <label class="cli-ask">
        <input v-model="allowAsk" :disabled="busy" type="checkbox" data-cli-allow-ask />
        {{ t('tauriPreview.nativeCli.allowAsk') }}
      </label>
      <AppButton :disabled="busy" @click="start">{{ t('tauriPreview.nativeCli.start') }}</AppButton>
    </template>
  </section>
</template>

<style scoped src="../panel.css"></style>
<style scoped>
.cli-ask,
.native-cli-endpoint {
  display: block;
  margin-block-end: var(--space-4);
  overflow-wrap: anywhere;
  user-select: all;
}
</style>
