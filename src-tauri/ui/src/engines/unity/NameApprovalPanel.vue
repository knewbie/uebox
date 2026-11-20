<script setup lang="ts">
import NameApprovalPanel from '../../agent/NameApprovalPanel.vue'

defineProps<{ agentId: string }>()
</script>

<template>
  <NameApprovalPanel engine="unity" :agent-id="agentId" />
</template>
