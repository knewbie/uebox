<script setup lang="ts">
import { computed } from 'vue'
import EditorAgentPanel from '../../agent/AgentPanel.vue'

const props = defineProps<{ sessionId: string; ready: boolean }>()
const target = computed(() => (props.ready ? { sessionId: props.sessionId } : null))
</script>

<template>
  <EditorAgentPanel engine="godot" :target-label="sessionId" :target="target" />
</template>
