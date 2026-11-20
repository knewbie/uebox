<script setup lang="ts">
import { onBeforeUnmount, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import AppAlert from '@renderer/components/AppAlert.vue'
import AppButton from '@renderer/components/AppButton.vue'
import { listNotebookSources, readNotebookSource, type NotebookSource } from './commands'

const props = defineProps<{ notebookId: string; disabled: boolean }>()
const { t } = useI18n()
const items = ref<NotebookSource[]>([])
const nextId = ref<string | null>(null)
const loaded = ref(false)
const loading = ref(false)
const failed = ref(false)
const selected = ref<string | null>(null)
const body = ref<string | null>(null)
const reading = ref(false)
const failedBody = ref(false)
let bodyRequest = 0
let disposed = false

onBeforeUnmount(() => {
  disposed = true
})

async function list(afterId: string | null): Promise<void> {
  if (loading.value || props.disabled) return
  loading.value = true
  failed.value = false
  try {
    const page = await listNotebookSources(props.notebookId, afterId)
    if (disposed) return
    items.value = [...items.value, ...page.items]
    nextId.value = page.nextId
    loaded.value = true
  } catch {
    if (!disposed) failed.value = true
  } finally {
    if (!disposed) loading.value = false
  }
}

async function open(sourceId: string): Promise<void> {
  if (props.disabled) return
  const request = ++bodyRequest
  selected.value = sourceId
  body.value = null
  reading.value = true
  failedBody.value = false
  try {
    const result = await readNotebookSource(props.notebookId, sourceId)
    if (!disposed && bodyRequest === request) body.value = result.content
  } catch {
    if (!disposed && bodyRequest === request) failedBody.value = true
  } finally {
    if (!disposed && bodyRequest === request) reading.value = false
  }
}

function close(): void {
  bodyRequest += 1
  selected.value = null
}
</script>

<template>
  <section aria-labelledby="notes-imported-sources">
    <h4 id="notes-imported-sources">{{ t('tauriPreview.notes.sourcesTitle') }}</h4>
    <AppButton
      v-if="!loaded"
      data-notebook-sources
      :disabled="disabled || loading"
      @click="list(null)"
      >{{ t('tauriPreview.notes.loadSources') }}</AppButton
    >
    <p v-if="loading" role="status">{{ t('tauriPreview.notes.loading') }}</p>
    <AppAlert v-if="failed" type="error" :message="t('tauriPreview.notes.sourcesError')" />
    <p v-if="loaded && !items.length">{{ t('tauriPreview.notes.emptySources') }}</p>
    <ul v-if="items.length" class="notes-list">
      <li v-for="item in items" :key="item.id">
        <AppButton
          :data-notebook-source="item.id"
          variant="text"
          :aria-pressed="selected === item.id"
          :disabled="disabled"
          @click="open(item.id)"
          >{{ item.title || item.fileName || t('tauriPreview.notes.untitled') }}</AppButton
        >
      </li>
    </ul>
    <AppButton v-if="nextId !== null" :disabled="disabled || loading" @click="list(nextId)">{{
      t('tauriPreview.notes.more')
    }}</AppButton>
    <div v-if="selected !== null">
      <p v-if="reading" role="status">{{ t('tauriPreview.notes.loading') }}</p>
      <AppAlert
        v-else-if="failedBody"
        type="error"
        :message="t('tauriPreview.notes.sourceError')"
      />
      <pre v-else-if="body" class="notes-source-text">{{ body }}</pre>
      <p v-else>{{ t('tauriPreview.notes.emptySourceText') }}</p>
      <AppButton @click="close">{{ t('tauriPreview.notes.closeSource') }}</AppButton>
    </div>
  </section>
</template>

<style scoped src="./notes.css"></style>
<style scoped>
.notes-source-text {
  overflow: auto;
  max-block-size: 20rem;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}
</style>
