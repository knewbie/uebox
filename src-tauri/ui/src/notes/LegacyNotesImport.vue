<script setup lang="ts">
import { invoke } from '@tauri-apps/api/core'
import { ref, shallowRef } from 'vue'
import { useI18n } from 'vue-i18n'
import { z } from 'zod'
import AppAlert from '@renderer/components/AppAlert.vue'
import AppButton from '@renderer/components/AppButton.vue'
import { readLegacyManifest, ManifestReadError } from '../legacy-history/manifest'
import { MAX_NOTES_MANIFEST_BYTES } from './commands'

const count = z.number().int().nonnegative()
const reportSchema = z.object({
  notebooksInserted: count,
  notebooksExisting: count,
  notesInserted: count,
  notesExisting: count,
  attachmentsInserted: count.optional(),
  originalFilesInserted: count.optional(),
  originalFilesExisting: count.optional()
})

const { t } = useI18n()
const emit = defineEmits<{ imported: [] }>()
const input = ref<HTMLInputElement | null>(null)
const file = shallowRef<File | null>(null)
const importing = ref(false)
const error = ref<string | null>(null)
const report = ref<z.infer<typeof reportSchema> | null>(null)

function selectFile(event: Event): void {
  file.value = (event.target as HTMLInputElement).files?.[0] ?? null
  report.value = null
  error.value = null
}

async function importFile(): Promise<void> {
  const selected = file.value
  if (!selected || importing.value) return
  importing.value = true
  error.value = null
  try {
    const manifest = await readLegacyManifest(selected, MAX_NOTES_MANIFEST_BYTES)
    report.value = reportSchema.parse(await invoke('import_legacy_notes', { manifest }))
    file.value = null
    if (input.value) input.value.value = ''
    emit('imported')
  } catch (cause) {
    if (
      cause === 'notes_import_limit' ||
      (cause instanceof ManifestReadError && cause.code === 'tooLarge')
    )
      error.value = 'importLimit'
    else if (cause === 'notes_import_conflict') error.value = 'importConflict'
    else error.value = 'importFailed'
  } finally {
    importing.value = false
  }
}
</script>

<template>
  <div class="notes-import">
    <label for="legacy-notes-file">{{ t('tauriPreview.notes.importTitle') }}</label>
    <input
      id="legacy-notes-file"
      ref="input"
      type="file"
      accept=".json,application/json"
      :disabled="importing"
      @change="selectFile"
    />
    <template v-if="file">
      <p>{{ t('tauriPreview.notes.importConfirm', { name: file.name }) }}</p>
      <AppButton :disabled="importing" data-notes-import-confirm @click="importFile">
        {{ t(importing ? 'tauriPreview.notes.importing' : 'tauriPreview.notes.importAction') }}
      </AppButton>
    </template>
    <AppAlert v-if="error" type="error" :message="t(`tauriPreview.notes.${error}`)" />
    <p v-if="report" role="status">
      {{ t('tauriPreview.notes.imported', report) }}
      <span v-if="report.attachmentsInserted || report.originalFilesInserted">
        {{
          t('tauriPreview.notes.importedRich', {
            attachments: report.attachmentsInserted ?? 0,
            originals: report.originalFilesInserted ?? 0
          })
        }}
      </span>
    </p>
  </div>
</template>

<style scoped>
.notes-import {
  display: grid;
  justify-items: start;
  gap: var(--space-2);
  margin-block: var(--space-4);
}
</style>
