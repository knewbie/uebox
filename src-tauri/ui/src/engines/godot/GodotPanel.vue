<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import AInput from 'ant-design-vue/es/input/Input'
import AppAlert from '@renderer/components/AppAlert.vue'
import AppButton from '@renderer/components/AppButton.vue'
import AgentPanel from './AgentPanel.vue'
import {
  launchGodot,
  connectGodot,
  queryGodotProject,
  queryGodotScene,
  statusGodotSession,
  releaseGodotSession,
  type GodotProject,
  type GodotScene,
  type GodotState,
  type GodotStatus
} from './commands'

const props = defineProps<{ os: string | null }>()
const { t } = useI18n()
const executable = ref('')
const project = ref('')
const busy = ref(false)
const error = ref<string | null>(null)
const sessionId = ref<string | null>(null)
const state = ref<GodotState | 'unknown' | null>(null)
const exit = ref<GodotStatus['exit']>(null)
const projectResult = ref<GodotProject | null>(null)
const sceneResult = ref<GodotScene | null>(null)

function absolutePath(path: string): boolean {
  if (props.os === null || path.includes('\0')) return false
  if (props.os === 'windows') {
    const normalized = path.replace(/\\/g, '/')
    return (
      /^(?:[a-zA-Z]:\/|\/\/[^/]+\/[^/]+)/.test(normalized) && !normalized.split('/').includes('..')
    )
  }
  return path.startsWith('/') && !path.split('/').includes('..')
}

const canLaunch = computed(
  () =>
    absolutePath(executable.value) && absolutePath(project.value) && !busy.value && !sessionId.value
)

watch(
  [executable, project],
  () => {
    error.value = null
  },
  { flush: 'sync' }
)

async function launch(): Promise<void> {
  if (!canLaunch.value) return
  busy.value = true
  error.value = null
  try {
    const result = await launchGodot(executable.value, project.value)
    sessionId.value = result.sessionId
    state.value = result.state
  } catch (cause) {
    error.value = String(cause)
  } finally {
    busy.value = false
  }
}

async function perform<T>(
  operation: (id: string) => Promise<T>,
  apply: (result: T) => void
): Promise<void> {
  if (busy.value || !sessionId.value) return
  const id = sessionId.value
  busy.value = true
  error.value = null
  try {
    apply(await operation(id))
  } catch (cause) {
    error.value = String(cause)
    state.value = 'unknown'
    exit.value = null
    projectResult.value = null
    sceneResult.value = null
  } finally {
    busy.value = false
  }
}

function connect(): Promise<void> {
  return perform(connectGodot, (result) => {
    projectResult.value = result
    state.value = 'ready'
    exit.value = null
  })
}

function readProject(): Promise<void> {
  return perform(queryGodotProject, (result) => {
    projectResult.value = result
  })
}

function readScene(): Promise<void> {
  return perform(queryGodotScene, (result) => {
    sceneResult.value = result
  })
}

function refreshStatus(): Promise<void> {
  return perform(statusGodotSession, (result) => {
    state.value = result.state
    exit.value = result.exit
    if (result.state !== 'ready') {
      projectResult.value = null
      sceneResult.value = null
    }
  })
}

function release(): Promise<void> {
  if (!busy.value && sessionId.value) state.value = 'unknown'
  return perform(releaseGodotSession, () => {
    sessionId.value = null
    state.value = null
    exit.value = null
    projectResult.value = null
    sceneResult.value = null
  })
}
</script>

<template>
  <section class="panel" aria-labelledby="godot-heading">
    <h2 id="godot-heading">{{ t('tauriPreview.godot.title') }}</h2>
    <p class="section-description">{{ t('tauriPreview.godot.intro') }}</p>
    <p class="section-description">{{ t('tauriPreview.godot.releaseHint') }}</p>
    <p class="section-description">{{ t('tauriPreview.godot.connectionHint') }}</p>
    <form class="godot-form" @submit.prevent="launch">
      <label for="godot-executable">{{ t('tauriPreview.godot.executable') }}</label>
      <AInput
        id="godot-executable"
        v-model:value="executable"
        name="godot-executable"
        autocomplete="off"
        aria-describedby="godot-executable-hint"
        :disabled="busy || Boolean(sessionId)"
      />
      <p id="godot-executable-hint" class="godot-hint">
        {{ t('tauriPreview.godot.executableHint') }}
      </p>
      <label for="godot-project">{{ t('tauriPreview.godot.project') }}</label>
      <AInput
        id="godot-project"
        v-model:value="project"
        name="godot-project"
        autocomplete="off"
        :disabled="busy || Boolean(sessionId)"
      />
      <AppButton
        data-godot-launch
        html-type="submit"
        variant="primary"
        :disabled="!canLaunch"
        :loading="busy && !sessionId"
        >{{ t('tauriPreview.godot.launch') }}</AppButton
      >
    </form>
    <p v-if="busy" role="status">
      {{ t(sessionId ? 'tauriPreview.godot.working' : 'tauriPreview.godot.launching') }}
    </p>
    <AppAlert
      v-if="error"
      type="error"
      show-icon
      :message="t('tauriPreview.godot.error')"
      :description="error"
    />
    <div v-if="sessionId" class="godot-session">
      <h3>{{ t('tauriPreview.godot.session') }}</h3>
      <p class="godot-id">{{ sessionId }}</p>
      <p>{{ t('tauriPreview.godot.lastStatus') }}: {{ t(`tauriPreview.godot.state.${state}`) }}</p>
      <pre v-if="exit">{{ JSON.stringify(exit, null, 2) }}</pre>
      <div class="godot-actions">
        <AppButton data-godot-connect :disabled="busy || state !== 'starting'" @click="connect">{{
          t('tauriPreview.godot.connect')
        }}</AppButton>
        <AppButton data-godot-project :disabled="busy || state !== 'ready'" @click="readProject">{{
          t('tauriPreview.godot.readProject')
        }}</AppButton>
        <AppButton data-godot-scene :disabled="busy || state !== 'ready'" @click="readScene">{{
          t('tauriPreview.godot.readScene')
        }}</AppButton>
        <AppButton data-godot-status :disabled="busy" @click="refreshStatus">{{
          t('tauriPreview.godot.refreshStatus')
        }}</AppButton>
        <AppButton data-godot-release :disabled="busy" @click="release">{{
          t('tauriPreview.godot.release')
        }}</AppButton>
      </div>
      <div v-if="projectResult" class="godot-result">
        <h3>{{ t('tauriPreview.godot.projectResult') }}</h3>
        <pre>{{ JSON.stringify(projectResult, null, 2) }}</pre>
      </div>
      <div v-if="sceneResult" class="godot-result">
        <h3>{{ t('tauriPreview.godot.sceneResult') }}</h3>
        <pre>{{ JSON.stringify(sceneResult, null, 2) }}</pre>
      </div>
      <AgentPanel :session-id="sessionId" :ready="state === 'ready'" />
    </div>
  </section>
</template>

<style scoped src="../../panel.css"></style>
<style scoped>
.godot-form {
  display: grid;
  gap: var(--space-3);
  margin-bottom: var(--space-5);
}
.godot-form :deep(.ant-input) {
  color: var(--color-text-primary);
  background: var(--color-bg-page);
  border-color: var(--color-border);
}
.godot-form :deep(.ant-input:focus-visible) {
  outline: 2px solid var(--color-border-focus);
  outline-offset: 2px;
}
.godot-form .app-button {
  justify-self: start;
}
.godot-hint {
  margin: 0;
  color: var(--color-text-secondary);
  font-size: var(--font-size-sm);
}
.godot-session h3 {
  margin: var(--space-6) 0 var(--space-4);
  font-size: var(--font-size-md);
}
.godot-id {
  overflow-wrap: anywhere;
  font-family: var(--font-family-mono);
}
.godot-actions {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-2);
}
.godot-session pre {
  margin: var(--space-3) 0 0;
  padding: var(--space-4);
  border-radius: var(--radius-md);
  background: var(--color-bg-page);
  font-family: var(--font-family-mono);
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}
</style>
