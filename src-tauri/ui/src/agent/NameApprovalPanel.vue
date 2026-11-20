<script setup lang="ts">
import { invoke } from '@tauri-apps/api/core'
import { onMounted, onUnmounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import AppAlert from '@renderer/components/AppAlert.vue'
import AppButton from '@renderer/components/AppButton.vue'

interface Proposal {
  approvalId: string
  sceneHandle?: number
  gameObjectInstanceId?: number
  nodePath?: string
  scenePath?: string | null
  oldName: string
  newName: string
}

interface Receipt {
  approvalId: string
  status: 'applied' | 'mismatch' | 'not_executed' | 'unknown'
  actualName: string | null
}

const props = defineProps<{ agentId: string; engine: 'unity' | 'godot' }>()
const messages = `tauriPreview.${props.engine}.nameWrite`
const { t } = useI18n()
const proposals = ref<Proposal[]>([])
const receipts = ref<Receipt[]>([])
const sending = ref<string | null>(null)
const answerError = ref(false)
let polling = false
let disposed = false
let timer: ReturnType<typeof setInterval>

async function refresh(): Promise<void> {
  if (disposed || polling) return
  polling = true
  try {
    const activity = await invoke<{ approvals: Proposal[]; receipts: Receipt[] }>(
      `list_${props.engine}_name_activity`,
      { agentId: props.agentId }
    )
    if (disposed) return
    proposals.value = activity.approvals
    receipts.value = activity.receipts
  } catch {
    if (!disposed) proposals.value = []
  } finally {
    polling = false
  }
}

async function answer(approvalId: string, approved: boolean): Promise<void> {
  if (sending.value) return
  sending.value = approvalId
  answerError.value = false
  try {
    await invoke(`answer_${props.engine}_name_approval`, { approvalId, approved })
    proposals.value = proposals.value.filter((proposal) => proposal.approvalId !== approvalId)
  } catch {
    if (!disposed) answerError.value = true
  } finally {
    if (!disposed) {
      sending.value = null
      void refresh()
    }
  }
}

onMounted(() => {
  void refresh()
  timer = setInterval(refresh, 400)
})

onUnmounted(() => {
  disposed = true
  clearInterval(timer)
})
</script>

<template>
  <div v-if="proposals.length || receipts.length || answerError" class="name-activity">
    <h3>{{ t(`${messages}.title`) }}</h3>
    <AppAlert v-if="answerError" type="error" :message="t(`${messages}.expired`)" />
    <div v-for="proposal in proposals" :key="proposal.approvalId">
      <p>{{ t(`${messages}.change`, proposal) }}</p>
      <p v-if="engine === 'godot' && !proposal.scenePath">
        {{ t(`${messages}.unsavedScene`, { nodePath: proposal.nodePath ?? '' }) }}
      </p>
      <p v-else>{{ t(`${messages}.identity`, proposal) }}</p>
      <div class="name-actions">
        <AppButton
          variant="primary"
          :disabled="sending !== null"
          v-bind="{ [`data-${engine}-approve`]: true }"
          @click="answer(proposal.approvalId, true)"
        >
          {{ t(`${messages}.approve`) }}
        </AppButton>
        <AppButton
          :disabled="sending !== null"
          v-bind="{ [`data-${engine}-decline`]: true }"
          @click="answer(proposal.approvalId, false)"
        >
          {{ t(`${messages}.decline`) }}
        </AppButton>
      </div>
    </div>
    <ul v-if="receipts.length" aria-live="polite">
      <li v-for="receipt in receipts" :key="receipt.approvalId">
        {{
          t(`${messages}.status.${receipt.status}`, {
            name: receipt.actualName ?? ''
          })
        }}
      </li>
    </ul>
  </div>
</template>

<style scoped>
.name-activity {
  margin-block: var(--space-4);
}

.name-actions {
  display: flex;
  gap: var(--space-2);
}
</style>
