<script setup lang="ts">
import { invoke } from '@tauri-apps/api/core'
import { onMounted, onUnmounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import AppAlert from '@renderer/components/AppAlert.vue'
import AppButton from '@renderer/components/AppButton.vue'

interface Proposal {
  approvalId: string
  sceneHandle: number
  gameObjectInstanceId: number
  oldName: string
  newName: string
}

interface Receipt {
  approvalId: string
  status: 'applied' | 'mismatch' | 'not_executed' | 'unknown'
  actualName: string | null
}

const props = defineProps<{ agentId: string }>()
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
      'list_unity_name_activity',
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
    await invoke('answer_unity_name_approval', { approvalId, approved })
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
    <h3>{{ t('tauriPreview.unity.nameWrite.title') }}</h3>
    <AppAlert
      v-if="answerError"
      type="error"
      :message="t('tauriPreview.unity.nameWrite.expired')"
    />
    <div v-for="proposal in proposals" :key="proposal.approvalId">
      <p>{{ t('tauriPreview.unity.nameWrite.change', proposal) }}</p>
      <p>{{ t('tauriPreview.unity.nameWrite.identity', proposal) }}</p>
      <div class="name-actions">
        <AppButton
          variant="primary"
          :disabled="sending !== null"
          data-unity-approve
          @click="answer(proposal.approvalId, true)"
        >
          {{ t('tauriPreview.unity.nameWrite.approve') }}
        </AppButton>
        <AppButton
          :disabled="sending !== null"
          data-unity-decline
          @click="answer(proposal.approvalId, false)"
        >
          {{ t('tauriPreview.unity.nameWrite.decline') }}
        </AppButton>
      </div>
    </div>
    <ul v-if="receipts.length" aria-live="polite">
      <li v-for="receipt in receipts" :key="receipt.approvalId">
        {{
          t(`tauriPreview.unity.nameWrite.status.${receipt.status}`, {
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
