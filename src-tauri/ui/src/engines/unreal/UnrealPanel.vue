<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import AInput from 'ant-design-vue/es/input/Input'
import AppAlert from '@renderer/components/AppAlert.vue'
import AppButton from '@renderer/components/AppButton.vue'
import AppCheckbox from '@renderer/components/AppCheckbox.vue'
import EditorAgentPanel from '../../agent/AgentPanel.vue'
import type { AgentTarget } from '../../agent/commands'
import ActorApprovalPanel from './ActorApprovalPanel.vue'
import { isAbsoluteProjectPath } from '../projectPath'
import {
  startUnrealListener,
  stopUnrealListener,
  listUnrealInstances,
  queryUnrealProject,
  type UnrealListenerInfo,
  type UnrealInstance
} from './commands'

const props = defineProps<{ hostReady: boolean; os?: string | null }>()
const { t } = useI18n()
const unrealPort = ref('17860')
const unrealListener = ref<UnrealListenerInfo | null>(null)
const unrealInstances = ref<UnrealInstance[]>([])
const unrealSelected = ref<string | null>(null)
const agentProjectRoot = ref('')
const actorLabelChanges = ref(false)
const agentId = ref<string | null>(null)
const monitorApprovals = ref(false)
const unrealResult = ref<Record<string, unknown> | null>(null)
const unrealError = ref<string | null>(null)
const unrealSelectionLost = ref(false)
const unrealChanging = ref(false)
const unrealRefreshing = ref(false)
const unrealQuerying = ref(false)
let unrealEpoch = 0

const validUnrealPort = computed(() => {
  if (!/^\d{1,5}$/.test(unrealPort.value)) return false
  const port = Number(unrealPort.value)
  return port >= 1 && port <= 65535
})
const canStartUnreal = computed(
  () => props.hostReady && validUnrealPort.value && !unrealListener.value && !unrealChanging.value
)
const canQueryUnreal = computed(
  () =>
    Boolean(unrealListener.value && unrealSelected.value) &&
    !unrealChanging.value &&
    !unrealRefreshing.value &&
    !unrealQuerying.value
)

const interactiveSelection = computed(() =>
  unrealInstances.value.some(
    (instance) => instance.connection_id === unrealSelected.value && instance.interactive
  )
)
const agentTarget = computed<Extract<AgentTarget, { connectionId: string }> | null>(() => {
  if (
    !unrealSelected.value ||
    !interactiveSelection.value ||
    !isAbsoluteProjectPath(agentProjectRoot.value, props.os ?? null)
  )
    return null
  const target = { connectionId: unrealSelected.value, projectRoot: agentProjectRoot.value }
  return actorLabelChanges.value ? { ...target, allowActorLabelChanges: true } : target
})

watch(
  agentProjectRoot,
  () => {
    actorLabelChanges.value = false
  },
  { flush: 'sync' }
)

function agentChanged(id: string | null): void {
  agentId.value = id
  if (id && actorLabelChanges.value) monitorApprovals.value = true
}

async function refreshUnreal(): Promise<void> {
  if (
    !unrealListener.value ||
    unrealChanging.value ||
    unrealRefreshing.value ||
    unrealQuerying.value
  )
    return
  const epoch = unrealEpoch
  unrealRefreshing.value = true
  unrealError.value = null
  try {
    const instances = await listUnrealInstances()
    if (epoch !== unrealEpoch) return
    unrealInstances.value = instances
    if (
      unrealSelected.value &&
      !instances.some((item) => item.connection_id === unrealSelected.value)
    ) {
      unrealSelected.value = null
      agentProjectRoot.value = ''
      unrealResult.value = null
      unrealSelectionLost.value = true
    }
  } catch (error) {
    if (epoch === unrealEpoch) unrealError.value = String(error)
  } finally {
    if (epoch === unrealEpoch) unrealRefreshing.value = false
  }
}

async function startUnreal(): Promise<void> {
  if (!canStartUnreal.value) return
  unrealChanging.value = true
  unrealError.value = null
  unrealSelectionLost.value = false
  try {
    unrealListener.value = await startUnrealListener(Number(unrealPort.value))
    unrealEpoch += 1
  } catch (error) {
    unrealError.value = String(error)
  } finally {
    unrealChanging.value = false
  }
  if (unrealListener.value) await refreshUnreal()
}

function selectUnreal(connectionId: string): void {
  if (
    !unrealListener.value ||
    unrealChanging.value ||
    unrealRefreshing.value ||
    unrealQuerying.value
  )
    return
  if (!unrealInstances.value.some((item) => item.connection_id === connectionId)) return
  if (unrealSelected.value !== connectionId) agentProjectRoot.value = ''
  unrealSelected.value = connectionId
  unrealResult.value = null
  unrealError.value = null
  unrealSelectionLost.value = false
}

async function queryUnreal(): Promise<void> {
  if (!canQueryUnreal.value || !unrealSelected.value) return
  const epoch = unrealEpoch
  const selected = unrealSelected.value
  unrealQuerying.value = true
  unrealResult.value = null
  unrealError.value = null
  try {
    const result = await queryUnrealProject(selected)
    if (epoch === unrealEpoch) unrealResult.value = result
  } catch (error) {
    if (epoch === unrealEpoch) {
      unrealError.value = String(error)
      unrealSelected.value = null
      agentProjectRoot.value = ''
      unrealSelectionLost.value = false
    }
  } finally {
    if (epoch === unrealEpoch) unrealQuerying.value = false
  }
}

async function stopUnreal(): Promise<void> {
  if (!unrealListener.value || unrealChanging.value) return
  unrealEpoch += 1
  unrealChanging.value = true
  unrealRefreshing.value = false
  unrealQuerying.value = false
  unrealSelected.value = null
  agentProjectRoot.value = ''
  unrealResult.value = null
  unrealSelectionLost.value = false
  unrealError.value = null
  try {
    await stopUnrealListener()
    unrealListener.value = null
    unrealInstances.value = []
  } catch (error) {
    unrealError.value = String(error)
  } finally {
    unrealChanging.value = false
  }
}
</script>

<template>
  <section class="panel" aria-labelledby="unreal-heading">
    <h2 id="unreal-heading">{{ t('tauriPreview.unreal.title') }}</h2>
    <p class="section-description">{{ t('tauriPreview.unreal.intro') }}</p>
    <p class="section-description">{{ t('tauriPreview.unreal.trust') }}</p>
    <div class="unreal-controls">
      <label for="unreal-port">{{ t('tauriPreview.unreal.port') }}</label>
      <AInput
        id="unreal-port"
        v-model:value="unrealPort"
        name="unreal-port"
        inputmode="numeric"
        autocomplete="off"
        aria-describedby="unreal-port-hint"
        :disabled="Boolean(unrealListener) || unrealChanging"
      />
      <p id="unreal-port-hint" class="unreal-hint">{{ t('tauriPreview.unreal.portHint') }}</p>
      <div class="unreal-actions">
        <AppButton
          data-unreal-start
          variant="primary"
          :disabled="!canStartUnreal"
          @click="startUnreal"
          >{{ t('tauriPreview.unreal.start') }}</AppButton
        >
        <AppButton
          data-unreal-stop
          :disabled="!unrealListener || unrealChanging"
          @click="stopUnreal"
          >{{ t('tauriPreview.unreal.stop') }}</AppButton
        >
      </div>
    </div>
    <p role="status" class="section-description">
      {{
        unrealListener
          ? t('tauriPreview.unreal.listening', unrealListener)
          : t('tauriPreview.unreal.notListening')
      }}
    </p>
    <AppAlert
      v-if="unrealError"
      type="error"
      show-icon
      :message="t('tauriPreview.unreal.error')"
      :description="unrealError"
    />
    <template v-if="unrealListener">
      <div class="unreal-list-heading">
        <h3>{{ t('tauriPreview.unreal.instances') }}</h3>
        <AppButton
          data-unreal-refresh
          size="small"
          :loading="unrealRefreshing"
          :disabled="unrealChanging || unrealQuerying"
          @click="refreshUnreal"
          >{{ t('tauriPreview.unreal.refresh') }}</AppButton
        >
      </div>
      <p v-if="!unrealInstances.length" class="section-description">
        {{ t('tauriPreview.unreal.empty') }}
      </p>
      <ul v-else class="unreal-instances">
        <li v-for="instance in unrealInstances" :key="instance.connection_id">
          <div class="unreal-instance-details">
            <strong>{{ instance.project_name }}</strong>
            <span>{{ instance.project_path }}</span>
            <span
              >{{ instance.connection_id }} ·
              {{
                t(
                  instance.interactive
                    ? 'tauriPreview.unreal.interactive'
                    : 'tauriPreview.unreal.nonInteractive'
                )
              }}</span
            >
          </div>
          <AppButton
            :data-unreal-select="instance.connection_id"
            :aria-pressed="unrealSelected === instance.connection_id"
            :disabled="unrealChanging || unrealRefreshing || unrealQuerying"
            :variant="unrealSelected === instance.connection_id ? 'soft' : 'default'"
            @click="selectUnreal(instance.connection_id)"
            >{{
              t(
                unrealSelected === instance.connection_id
                  ? 'tauriPreview.unreal.selected'
                  : 'tauriPreview.unreal.select'
              )
            }}</AppButton
          >
        </li>
      </ul>
      <p v-if="unrealSelectionLost" role="status" class="section-description">
        {{ t('tauriPreview.unreal.selectionLost') }}
      </p>
      <AppButton
        data-unreal-query
        variant="primary"
        :disabled="!canQueryUnreal"
        :loading="unrealQuerying"
        @click="queryUnreal"
        >{{ t('tauriPreview.unreal.query') }}</AppButton
      >
      <p v-if="unrealQuerying" role="status">{{ t('tauriPreview.unreal.querying') }}</p>
      <div v-if="unrealResult" class="unreal-result">
        <h3>{{ t('tauriPreview.unreal.result') }}</h3>
        <pre>{{ JSON.stringify(unrealResult, null, 2) }}</pre>
      </div>
      <div v-if="interactiveSelection" class="unreal-controls">
        <label for="unreal-agent-project">{{ t('tauriPreview.unreal.assistantRoot') }}</label>
        <AInput
          id="unreal-agent-project"
          v-model:value="agentProjectRoot"
          name="unreal-agent-project"
          autocomplete="off"
        />
        <AppCheckbox
          v-model:checked="actorLabelChanges"
          :disabled="Boolean(agentId) || !agentTarget"
        >
          {{ t('tauriPreview.unreal.actorApproval.optIn') }}
        </AppCheckbox>
      </div>
      <EditorAgentPanel
        v-if="agentTarget"
        engine="unreal"
        :target="agentTarget"
        :target-label="agentTarget.connectionId"
        @agent-changed="agentChanged"
      />
    </template>
    <ActorApprovalPanel :enabled="monitorApprovals" :agent-id="agentId" />
  </section>
</template>

<style scoped src="../../panel.css"></style>

<style scoped>
.unreal-hint {
  margin: 0;
  color: var(--color-text-secondary);
  font-size: var(--font-size-sm);
}
.unreal-controls {
  display: grid;
  gap: var(--space-3);
}
.unreal-controls :deep(.ant-input) {
  color: var(--color-text-primary);
  background: var(--color-bg-page);
  border-color: var(--color-border);
}
.unreal-controls :deep(.ant-input:focus-visible) {
  outline: 2px solid var(--color-border-focus);
  outline-offset: 2px;
}
.unreal-actions {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-2);
}
.unreal-list-heading {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-3);
  margin: var(--space-5) 0 var(--space-3);
}
.unreal-list-heading h3,
.unreal-result h3 {
  margin: 0;
  font-size: var(--font-size-md);
}
.unreal-instances {
  list-style: none;
  padding: 0;
  margin: 0 0 var(--space-5);
}
.unreal-instances li {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-4);
  padding: var(--space-3) 0;
  border-top: 1px solid var(--color-border-subtle);
}
.unreal-instance-details {
  display: grid;
  min-width: 0;
  gap: var(--space-1);
  overflow-wrap: anywhere;
}
.unreal-instance-details span {
  color: var(--color-text-secondary);
  font-size: var(--font-size-sm);
}
.unreal-result {
  margin-top: var(--space-5);
}
.unreal-result pre {
  margin: var(--space-3) 0 0;
  padding: var(--space-4);
  border-radius: var(--radius-md);
  background: var(--color-bg-page);
  font-family: var(--font-family-mono);
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}
</style>
