<script setup lang="ts">
import { ref } from 'vue'
import { useI18n } from 'vue-i18n'
import AppAlert from '@renderer/components/AppAlert.vue'
import AppButton from '@renderer/components/AppButton.vue'
import { exportNoteAttachment, type Note } from './commands'

const props = defineProps<{
  noteId: string
  attachments: NonNullable<Note['attachments']>
}>()
const { t } = useI18n()
const exporting = ref<string | null>(null)
const error = ref<string | null>(null)
const saved = ref(false)

async function save(id: string): Promise<void> {
  if (exporting.value) return
  exporting.value = id
  error.value = null
  saved.value = false
  try {
    saved.value = await exportNoteAttachment(props.noteId, id)
  } catch (cause) {
    error.value =
      cause instanceof Error && cause.message === 'notes_attachment_destination_exists'
        ? 'attachmentExists'
        : 'attachmentFailed'
  } finally {
    exporting.value = null
  }
}
</script>

<template>
  <section class="note-attachments">
    <h4>{{ t('tauriPreview.notes.attachments') }}</h4>
    <ul>
      <li v-for="attachment in attachments" :key="attachment.id">
        <span>{{ attachment.fileName ?? t('tauriPreview.notes.unnamedAttachment') }}</span>
        <AppButton :disabled="exporting !== null" @click="save(attachment.id)">
          {{ t('tauriPreview.notes.exportAttachment') }}
        </AppButton>
      </li>
    </ul>
    <p v-if="saved" role="status">{{ t('tauriPreview.notes.attachmentSaved') }}</p>
    <AppAlert v-if="error" type="error" :message="t(`tauriPreview.notes.${error}`)" />
  </section>
</template>

<style scoped>
.note-attachments {
  margin-block: var(--space-4);
}
.note-attachments li {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-2);
  overflow-wrap: anywhere;
}
</style>
