<script setup lang="ts">
import { computed, onUnmounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import AInput from 'ant-design-vue/es/input/Input'
import ATextarea from 'ant-design-vue/es/input/TextArea'
import AppAlert from '@renderer/components/AppAlert.vue'
import AppButton from '@renderer/components/AppButton.vue'
import {
  cancelAgent,
  closeAgent,
  createAgent,
  pollAgent,
  startAgent,
  type AgentPhase
} from './agentCommands'

const props = defineProps<{ sessionId: string; ready: boolean }>()
const { t } = useI18n()
const endpoint = ref('')
const model = ref('')
const credential = ref('')
const message = ref('')
const agentId = ref<string | null>(null)
const runId = ref<number | null>(null)
const phase = ref<AgentPhase | null>(null)
const busy = ref(false)
const cancelled = ref(false)
const error = ref<string | null>(null)
const messages = ref<{ role: 'user' | 'assistant'; text: string }[]>([])
let epoch = 0
let timer: ReturnType<typeof setTimeout> | undefined

const active = computed(
  () => phase.value === 'running' || phase.value === 'cancelling' || phase.value === 'draining'
)
const canCreate = computed(
  () =>
    props.ready &&
    !agentId.value &&
    !busy.value &&
    Boolean(endpoint.value.trim()) &&
    Boolean(model.value.trim())
)
const canSend = computed(
  () =>
    props.ready &&
    Boolean(agentId.value) &&
    !busy.value &&
    !active.value &&
    Boolean(message.value.trim())
)
const knownErrors = new Set([
  'target_unavailable',
  'agent_unavailable',
  'agent_already_exists',
  'agent_busy',
  'invalid_model_configuration',
  'invalid_message',
  'model_network_error',
  'model_http_error',
  'model_protocol_error',
  'model_output_limit',
  'model_provider_error',
  'agent_step_limit',
  'agent_context_limit',
  'agent_output_buffer_limit',
  'credential_unavailable'
])
function displayError(cause: unknown): void {
  const code = typeof cause === 'string' && knownErrors.has(cause) ? cause : 'generic'
  error.value = t(`tauriPreview.godotAgent.errors.${code}`)
}
function reset(): void {
  epoch += 1
  if (timer) clearTimeout(timer)
  timer = undefined
  const previous = agentId.value
  agentId.value = null
  runId.value = null
  phase.value = null
  busy.value = false
  cancelled.value = false
  credential.value = ''
  messages.value = []
  error.value = null
  if (previous) void closeAgent(previous).catch(() => {})
}
watch(
  () => [props.sessionId, props.ready] as const,
  ([id, ready], [oldId, oldReady]) => {
    if (id !== oldId || (oldReady && !ready)) reset()
  },
  { flush: 'sync' }
)
onUnmounted(reset)

async function configure(): Promise<void> {
  if (!canCreate.value) return
  const current = epoch
  const target = props.sessionId
  const secret = credential.value || null
  credential.value = ''
  busy.value = true
  error.value = null
  try {
    const result = await createAgent(target, endpoint.value.trim(), model.value.trim(), secret)
    if (current !== epoch || !props.ready || props.sessionId !== target) {
      void closeAgent(result.agentId).catch(() => {})
      return
    }
    agentId.value = result.agentId
  } catch (cause) {
    if (current === epoch) displayError(cause)
  } finally {
    if (current === epoch) busy.value = false
  }
}

async function poll(id: string, run: number, current: number): Promise<void> {
  try {
    const update = await pollAgent(id, run)
    if (current !== epoch || agentId.value !== id || runId.value !== run) return
    if (update.agentId === id && update.runId === run) {
      if (!cancelled.value) {
        const last = messages.value.at(-1)
        if (last?.role === 'assistant') last.text += update.fragments.join('')
      }
      phase.value = cancelled.value && update.phase === 'running' ? 'cancelling' : update.phase
      if (update.error) displayError(update.error)
    }
    if (active.value) timer = setTimeout(() => void poll(id, run, current), 150)
  } catch (cause) {
    if (current !== epoch || agentId.value !== id || runId.value !== run) return
    displayError(cause)
    // A failed poll cannot prove that a run has drained.
    phase.value = 'draining'
    timer = setTimeout(() => void poll(id, run, current), 500)
  }
}
async function send(): Promise<void> {
  if (!canSend.value || !agentId.value) return
  const id = agentId.value
  const current = epoch
  const text = message.value
  message.value = ''
  busy.value = true
  error.value = null
  cancelled.value = false
  try {
    const result = await startAgent(id, text)
    if (current !== epoch || agentId.value !== id || result.agentId !== id) {
      void cancelAgent(result.agentId, result.runId).catch(() => {})
      return
    }
    messages.value.push({ role: 'user', text }, { role: 'assistant', text: '' })
    runId.value = result.runId
    phase.value = 'running'
    void poll(id, result.runId, current)
  } catch (cause) {
    if (current === epoch) displayError(cause)
  } finally {
    if (current === epoch) busy.value = false
  }
}
async function cancel(): Promise<void> {
  if (!agentId.value || runId.value === null || !active.value || cancelled.value) return
  const id = agentId.value
  const run = runId.value
  const current = epoch
  cancelled.value = true
  phase.value = 'cancelling'
  try {
    await cancelAgent(id, run)
  } catch (cause) {
    if (current === epoch) displayError(cause)
  }
}
</script>

<template>
  <section class="agent" aria-labelledby="godot-agent-heading">
    <h3 id="godot-agent-heading">{{ t('tauriPreview.godotAgent.title') }}</h3>
    <p class="agent-hint">{{ t('tauriPreview.godotAgent.intro') }}</p>
    <p>
      {{ t('tauriPreview.godotAgent.target') }}: <code>{{ sessionId }}</code>
    </p>
    <p v-if="!ready" role="status">{{ t('tauriPreview.godotAgent.unavailable') }}</p>
    <form class="agent-form" @submit.prevent="configure">
      <label for="agent-endpoint">{{ t('tauriPreview.godotAgent.endpoint') }}</label>
      <AInput
        id="agent-endpoint"
        v-model:value="endpoint"
        name="agent-endpoint"
        autocomplete="off"
        :disabled="busy || Boolean(agentId)"
      />
      <label for="agent-model">{{ t('tauriPreview.godotAgent.model') }}</label>
      <AInput
        id="agent-model"
        v-model:value="model"
        name="agent-model"
        autocomplete="off"
        :disabled="busy || Boolean(agentId)"
      />
      <label for="agent-credential">{{ t('tauriPreview.godotAgent.credential') }}</label>
      <AInput
        id="agent-credential"
        v-model:value="credential"
        name="agent-credential"
        type="password"
        autocomplete="off"
        :disabled="busy || Boolean(agentId)"
      />
      <div class="agent-actions">
        <AppButton data-agent-create html-type="submit" variant="primary" :disabled="!canCreate">{{
          t('tauriPreview.godotAgent.create')
        }}</AppButton>
        <AppButton v-if="agentId" data-agent-close :disabled="busy" @click="reset">{{
          t('tauriPreview.godotAgent.close')
        }}</AppButton>
      </div>
    </form>
    <AppAlert
      v-if="error"
      type="error"
      show-icon
      :message="t('tauriPreview.godotAgent.error')"
      :description="error"
    />
    <div v-if="agentId" class="agent-chat">
      <div v-for="(item, index) in messages" :key="index" class="agent-message">
        <strong>{{ t(`tauriPreview.godotAgent.${item.role}`) }}</strong>
        <p>{{ item.text }}</p>
      </div>
      <p v-if="phase" role="status">{{ t(`tauriPreview.godotAgent.phase.${phase}`) }}</p>
      <form class="agent-form" @submit.prevent="send">
        <label for="agent-message">{{ t('tauriPreview.godotAgent.message') }}</label>
        <ATextarea
          id="agent-message"
          v-model:value="message"
          name="agent-message"
          :disabled="!ready || busy || active"
        />
        <div class="agent-actions">
          <AppButton data-agent-send html-type="submit" variant="primary" :disabled="!canSend">{{
            t('tauriPreview.godotAgent.send')
          }}</AppButton>
          <AppButton data-agent-cancel :disabled="!active || cancelled" @click="cancel">{{
            t('tauriPreview.godotAgent.cancel')
          }}</AppButton>
        </div>
      </form>
    </div>
  </section>
</template>

<style scoped>
.agent {
  margin-top: var(--space-6);
  padding-top: var(--space-4);
  border-top: 1px solid var(--color-border-subtle);
}
.agent-hint {
  color: var(--color-text-secondary);
  line-height: var(--line-height-normal);
}
.agent code {
  overflow-wrap: anywhere;
}
.agent-form {
  display: grid;
  gap: var(--space-3);
  margin: var(--space-4) 0;
}
.agent-form :deep(.ant-input) {
  color: var(--color-text-primary);
  background: var(--color-bg-page);
  border-color: var(--color-border);
}
.agent-form :deep(.ant-input:focus-visible) {
  outline: 2px solid var(--color-border-focus);
  outline-offset: 2px;
}
.agent-actions {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-2);
}
.agent-message {
  margin-top: var(--space-4);
}
.agent-message p {
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}
</style>
