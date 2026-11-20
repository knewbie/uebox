<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import EditorAgentPanel from '../../agent/AgentPanel.vue'
import NameApprovalPanel from '../../agent/NameApprovalPanel.vue'
import type { AgentTarget } from '../../agent/commands'

const props = defineProps<{ sessionId: string; ready: boolean }>()
const { t } = useI18n()
const optIntoRename = ref(false)
const agentId = ref<string | null>(null)
watch(
  () => props.sessionId,
  () => {
    optIntoRename.value = false
  },
  { flush: 'sync' }
)
const target = computed<AgentTarget | null>(() => {
  if (!props.ready) return null
  return {
    sessionId: props.sessionId,
    ...(optIntoRename.value ? { allowNodeRename: true } : {})
  }
})
</script>

<template>
  <label v-if="ready">
    <input
      v-model="optIntoRename"
      data-godot-opt-into-rename
      type="checkbox"
      :disabled="agentId !== null"
    />
    {{ t('tauriPreview.godot.nameWrite.optIn') }}
  </label>
  <EditorAgentPanel
    engine="godot"
    :target-label="sessionId"
    :target="target"
    @agent-changed="agentId = $event"
  />
  <NameApprovalPanel
    v-if="optIntoRename && agentId"
    :key="agentId"
    engine="godot"
    :agent-id="agentId"
  />
</template>
