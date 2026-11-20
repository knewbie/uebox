<script setup lang="ts">
import { invoke } from '@tauri-apps/api/core'
import AppAlert from '@renderer/components/AppAlert.vue'
import AppButton from '@renderer/components/AppButton.vue'
import { onUnmounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'

const props = defineProps<{ enabled: boolean; agentId: string | null }>()
const { t } = useI18n()

interface Proposal {
  approvalId: string
  actorPath: string
  oldLabel: string
  newLabel: string
}
interface Receipt {
  approvalId: string
  status: 'applied' | 'mismatch' | 'not_executed' | 'unknown'
  actualLabel: string | null
  historyError?: string
}
interface ActorApprovalSnapshot {
  approvals: Proposal[]
  receipts: Receipt[]
}

const approvals = ref<Proposal[]>([])
const receipts = ref<Receipt[]>([])
const answering = ref(false)
const decisionError = ref(false)
let epoch = 0
let request = 0
let timer: ReturnType<typeof setTimeout> | undefined

async function readSnapshot(current: number): Promise<void> {
  const issued = ++request
  const snapshot = await invoke<ActorApprovalSnapshot>('list_unreal_actor_approvals', {
    agentId: props.agentId
  })
  if (current !== epoch || issued !== request) return
  approvals.value = snapshot.approvals
  receipts.value = snapshot.receipts
}

async function poll(current: number): Promise<void> {
  try {
    await readSnapshot(current)
  } catch {
    // Keep completed receipts while the host is temporarily unavailable.
    if (current === epoch) approvals.value = []
  } finally {
    if (current === epoch) timer = setTimeout(() => void poll(current), 350)
  }
}

function stop(): void {
  epoch += 1
  if (timer !== undefined) clearTimeout(timer)
  timer = undefined
}

watch(
  () => [props.enabled, props.agentId] as const,
  ([enabled]) => {
    stop()
    approvals.value = []
    decisionError.value = false
    if (enabled) void poll(epoch)
  },
  { immediate: true }
)
onUnmounted(stop)

async function answer(approvalId: string, allowed: boolean): Promise<void> {
  if (answering.value) return
  answering.value = true
  decisionError.value = false
  const current = epoch
  try {
    await invoke('answer_unreal_actor_approval', { approvalId, allowed })
    if (current === epoch) {
      approvals.value = approvals.value.filter((approval) => approval.approvalId !== approvalId)
    }
  } catch {
    if (current === epoch) decisionError.value = true
  } finally {
    try {
      if (current === epoch) await readSnapshot(current)
    } catch {
      // A decision and its eventual receipt are separate operations.
    } finally {
      answering.value = false
    }
  }
}
</script>

<template>
  <section v-if="approvals.length || receipts.length || decisionError" class="actor-approvals">
    <h3>{{ t('tauriPreview.unreal.actorApproval.title') }}</h3>
    <AppAlert
      v-if="decisionError"
      type="warning"
      show-icon
      :message="t('tauriPreview.unreal.actorApproval.unavailable')"
    />
    <article v-for="approval in approvals" :key="approval.approvalId" class="actor-approval">
      <code>{{ approval.actorPath }}</code>
      <p>{{ t('tauriPreview.unreal.actorApproval.previous', { label: approval.oldLabel }) }}</p>
      <p>{{ t('tauriPreview.unreal.actorApproval.proposed', { label: approval.newLabel }) }}</p>
      <div class="actor-approval-actions">
        <AppButton
          data-actor-approve
          variant="primary"
          :disabled="answering"
          @click="answer(approval.approvalId, true)"
          >{{ t('tauriPreview.unreal.actorApproval.approve') }}</AppButton
        >
        <AppButton
          data-actor-deny
          :disabled="answering"
          @click="answer(approval.approvalId, false)"
        >
          {{ t('tauriPreview.unreal.actorApproval.deny') }}
        </AppButton>
      </div>
    </article>
    <ul v-if="receipts.length" class="actor-approval-receipts">
      <li v-for="receipt in receipts" :key="receipt.approvalId">
        {{
          t(`tauriPreview.unreal.actorApproval.receipt.${receipt.status}`, {
            label: receipt.actualLabel
          })
        }}
        <span v-if="receipt.historyError">
          {{ t('tauriPreview.unreal.actorApproval.historyError') }}
        </span>
      </li>
    </ul>
  </section>
</template>

<style scoped>
.actor-approvals {
  border-top: 1px solid var(--color-border-subtle);
  padding-top: var(--space-4);
  margin-top: var(--space-4);
  overflow-wrap: anywhere;
}
.actor-approval + .actor-approval {
  margin-top: var(--space-4);
}
.actor-approval-actions {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-2);
}
.actor-approval-receipts {
  padding-inline-start: var(--space-5);
}
</style>
