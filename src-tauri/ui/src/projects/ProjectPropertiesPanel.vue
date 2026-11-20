<script setup lang="ts">
import { invoke } from '@tauri-apps/api/core'
import { onBeforeUnmount, onMounted, ref, shallowRef } from 'vue'
import { useI18n } from 'vue-i18n'
import AppAlert from '@renderer/components/AppAlert.vue'
import AppButton from '@renderer/components/AppButton.vue'
import type { RegisteredProject } from './commands'

interface Fields {
  note: string | null
  description: string | null
  isPinned: boolean
  sortOrder: number
}

interface Properties {
  fields: Fields
  hasCover: boolean
}

interface Cover {
  mime: 'image/png' | 'image/jpeg' | 'image/webp'
  data: string
}

const props = defineProps<{ project: RegisteredProject }>()
const emit = defineEmits<{ updated: []; close: [] }>()
const { t } = useI18n()
const fields = ref<Fields | null>(null)
const preview = ref<string | null>(null)
const hasCover = ref(false)
const picture = shallowRef<File | null>(null)
const busy = ref(false)
const error = ref<string | null>(null)
const saved = ref(false)
let disposed = false
const maxCoverBytes = 8 * 1024 * 1024

function useCover(value: Cover | null): void {
  preview.value =
    value && ['image/png', 'image/jpeg', 'image/webp'].includes(value.mime)
      ? `data:${value.mime};base64,${value.data}`
      : null
}

async function run(operation: () => Promise<void>): Promise<void> {
  if (busy.value || disposed) return
  busy.value = true
  error.value = null
  saved.value = false
  try {
    await operation()
  } catch (cause) {
    if (!disposed) error.value = cause === 'project_cover_invalid' ? 'invalidCover' : 'error'
  } finally {
    if (!disposed) busy.value = false
  }
}

function load(): Promise<void> {
  return run(async () => {
    const selected = { project: props.project.id }
    const result = await invoke<Properties>('get_project_properties', selected)
    if (disposed) return
    fields.value = result.fields
    hasCover.value = result.hasCover
    if (!hasCover.value) return
    const data = await invoke<Cover | null>('read_project_cover', selected)
    if (!disposed) useCover(data)
  })
}

function saveFields(): Promise<void> {
  return run(async () => {
    if (!fields.value || !Number.isFinite(fields.value.sortOrder)) return
    const updated = await invoke<Fields>('save_project_properties', {
      project: props.project.id,
      fields: { ...fields.value }
    })
    if (disposed) return
    fields.value = updated
    saved.value = true
    emit('updated')
  })
}

function selectCover(event: Event): void {
  picture.value = (event.target as HTMLInputElement).files?.[0] ?? null
}

function bytes(file: File): Promise<string> {
  if (!file.size || file.size > maxCoverBytes) return Promise.reject('project_cover_invalid')
  return new Promise((resolve, reject) => {
    const reader = new FileReader()
    reader.onload = () => {
      const result = reader.result
      if (typeof result !== 'string' || !result.includes(',')) reject('project_cover_invalid')
      else resolve(result.slice(result.indexOf(',') + 1))
    }
    reader.onerror = () => reject('project_cover_invalid')
    reader.readAsDataURL(file)
  })
}

function saveCover(): Promise<void> {
  const file = picture.value
  if (!file) return Promise.resolve()
  return run(async () => {
    const encoded = await bytes(file)
    if (disposed) return
    const data = await invoke<Cover>('save_project_cover', { project: props.project.id, encoded })
    if (disposed) return
    useCover(data)
    hasCover.value = true
    picture.value = null
    saved.value = true
  })
}

function removeCover(): Promise<void> {
  return run(async () => {
    await invoke('remove_project_cover', { project: props.project.id })
    if (disposed) return
    useCover(null)
    hasCover.value = false
    saved.value = true
  })
}

onMounted(load)
onBeforeUnmount(() => {
  disposed = true
})
</script>

<template>
  <section class="project-properties" aria-labelledby="project-properties-title">
    <h3 id="project-properties-title">
      {{ t('tauriPreview.projectLibrary.properties.title', { name: project.name }) }}
    </h3>
    <AppAlert
      v-if="error"
      type="error"
      :message="t(`tauriPreview.projectLibrary.properties.${error}`)"
    />
    <p v-if="saved" role="status">{{ t('tauriPreview.projectLibrary.properties.saved') }}</p>
    <form v-if="fields" class="project-properties-form" @submit.prevent="saveFields">
      <label for="project-desc">{{
        t('tauriPreview.projectLibrary.properties.description')
      }}</label>
      <textarea
        id="project-desc"
        v-model="fields.description"
        maxlength="4096"
        rows="3"
        :disabled="busy"
      />
      <label for="project-note">{{ t('tauriPreview.projectLibrary.properties.note') }}</label>
      <textarea
        id="project-note"
        v-model="fields.note"
        maxlength="4096"
        rows="3"
        :disabled="busy"
      />
      <label for="project-order">{{ t('tauriPreview.projectLibrary.properties.order') }}</label>
      <input
        id="project-order"
        v-model.number="fields.sortOrder"
        type="number"
        step="any"
        :disabled="busy"
      />
      <label>
        <input v-model="fields.isPinned" type="checkbox" :disabled="busy" />
        {{ t('tauriPreview.projectLibrary.properties.pin') }}
      </label>
      <AppButton html-type="submit" :disabled="busy">{{
        t('tauriPreview.projectLibrary.properties.save')
      }}</AppButton>
    </form>
    <form v-if="fields" class="project-properties-form" @submit.prevent="saveCover">
      <img
        v-if="preview"
        class="project-cover-preview"
        :src="preview"
        :alt="t('tauriPreview.projectLibrary.properties.cover')"
      />
      <label for="project-cover">{{ t('tauriPreview.projectLibrary.properties.cover') }}</label>
      <input
        id="project-cover"
        type="file"
        accept="image/png,image/jpeg,image/webp"
        :disabled="busy"
        @change="selectCover"
      />
      <div class="project-properties-actions">
        <AppButton html-type="submit" :disabled="busy || !picture">{{
          t('tauriPreview.projectLibrary.properties.saveCover')
        }}</AppButton>
        <AppButton v-if="hasCover" :disabled="busy" @click="removeCover">{{
          t('tauriPreview.projectLibrary.properties.removeCover')
        }}</AppButton>
      </div>
    </form>
    <AppButton v-if="!fields && error" :disabled="busy" @click="load">{{
      t('tauriPreview.projectLibrary.refresh')
    }}</AppButton>
    <AppButton :disabled="busy" @click="emit('close')">{{
      t('tauriPreview.projectLibrary.properties.close')
    }}</AppButton>
  </section>
</template>

<style scoped>
.project-properties {
  margin-block: var(--space-5);
}
.project-properties-form {
  display: grid;
  justify-items: start;
  gap: var(--space-2);
  margin-block: var(--space-4);
}
.project-properties-form textarea {
  width: min(100%, calc(var(--space-12) * 10));
}
.project-cover-preview {
  max-height: 12rem;
  max-width: 100%;
  object-fit: contain;
}
.project-properties-actions {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-2);
}
</style>
