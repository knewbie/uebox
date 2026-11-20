<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import AInput from 'ant-design-vue/es/input/Input'
import AppAlert from '@renderer/components/AppAlert.vue'
import AppButton from '@renderer/components/AppButton.vue'
import AppCheckbox from '@renderer/components/AppCheckbox.vue'
import type { RegisteredProject } from '../commands'
import {
  createProjectCollection,
  importProjectCollections,
  listProjectCollections,
  moveProjectCollection,
  removeProjectCollection,
  renameProjectCollection,
  setProjectCollectionMembership,
  type CollectionDirection,
  type ImportedCollections,
  type ProjectCollection
} from './commands'

const props = defineProps<{ projects: readonly RegisteredProject[] }>()
const { t } = useI18n()
const collections = ref<ProjectCollection[]>([])
const selectedId = ref<string | null>(null)
const selected = computed(() => collections.value.find((item) => item.id === selectedId.value))
const newName = ref('')
const renameDraft = ref('')
const pendingRemoval = ref<string | null>(null)
const removal = computed(() => collections.value.find((item) => item.id === pendingRemoval.value))
const pending = ref<'load' | 'change' | null>(null)
const loaded = ref(false)
const error = ref<'loadError' | 'changeError' | 'importError' | 'importMissingProject' | null>(null)
const importInput = ref<HTMLInputElement | null>(null)
const selectedFile = ref<File | null>(null)
const imported = ref<ImportedCollections | null>(null)
const busy = computed(() => pending.value !== null)
const canRename = computed(
  () => selected.value && normalizedName(renameDraft.value) !== selected.value.name
)
const missingProjectIds = computed(() => {
  const registered = new Set(props.projects.map((project) => project.id))
  return selected.value?.projectIds.filter((id) => !registered.has(id)) ?? []
})
let disposed = false

watch([selectedId, () => selected.value?.name], ([, name]) => {
  renameDraft.value = name ?? ''
})

function normalizedName(value: string): string | null {
  return value.trim() || null
}

function displayName(collection: ProjectCollection): string {
  return collection.name?.trim() ? collection.name : t('tauriPreview.projectCollections.unnamed')
}

function selectCollection(id: string): void {
  selectedId.value = id
  renameDraft.value = selected.value?.name ?? ''
  pendingRemoval.value = null
}

async function perform(
  operation: 'load' | 'change',
  action: () => Promise<ProjectCollection[]>,
  after?: (next: ProjectCollection[]) => void
): Promise<void> {
  if (busy.value || disposed) return
  pending.value = operation
  error.value = null
  try {
    const next = await action()
    if (disposed) return
    // Every command returns the authoritative full list, including its order.
    collections.value = next
    loaded.value = true
    if (!next.some((item) => item.id === selectedId.value)) {
      selectedId.value = next[0]?.id ?? null
      pendingRemoval.value = null
    }
    after?.(next)
  } catch (cause) {
    if (disposed) return
    const code = cause instanceof Error ? cause.message : ''
    error.value =
      code === 'importError' || code === 'importMissingProject'
        ? code
        : operation === 'load'
          ? 'loadError'
          : 'changeError'
  } finally {
    if (!disposed) pending.value = null
  }
}

function refresh(): Promise<void> {
  return perform('load', listProjectCollections)
}

function selectImport(event: Event): void {
  selectedFile.value = (event.target as HTMLInputElement).files?.[0] ?? null
  imported.value = null
  error.value = null
}

function importSelected(): Promise<void> {
  const file = selectedFile.value
  if (!file) return Promise.resolve()
  return perform('change', async () => {
    if (file.size > 1024 * 1024) throw new Error('importError')
    let manifest: string
    try {
      manifest = new TextDecoder('utf-8', { fatal: true }).decode(await file.arrayBuffer())
    } catch {
      throw new Error('importError')
    }
    if (disposed) return []
    const result = await importProjectCollections(manifest)
    if (!disposed) {
      imported.value = result
      selectedFile.value = null
      if (importInput.value) importInput.value.value = ''
    }
    return result.collections
  })
}

function createCollection(): Promise<void> {
  if (!loaded.value) return Promise.resolve()
  const previousIds = new Set(collections.value.map((item) => item.id))
  const name = normalizedName(newName.value)
  return perform(
    'change',
    () => createProjectCollection(name),
    (next) => {
      newName.value = ''
      const added = next.filter((item) => !previousIds.has(item.id))
      if (added.length === 1) selectCollection(added[0].id)
    }
  )
}

function renameCollection(): Promise<void> {
  const collection = selected.value
  if (!collection || !canRename.value) return Promise.resolve()
  const name = normalizedName(renameDraft.value)
  return perform('change', () => renameProjectCollection(collection.id, name))
}

function moveCollection(collectionId: string, direction: CollectionDirection): Promise<void> {
  const index = collections.value.findIndex((item) => item.id === collectionId)
  if (index < 0 || (direction === 'up' ? index === 0 : index === collections.value.length - 1)) {
    return Promise.resolve()
  }
  return perform('change', () => moveProjectCollection(collectionId, direction))
}

function setMembership(registrationId: string, belongs: boolean): Promise<void> {
  const collection = selected.value
  if (!collection) return Promise.resolve()
  return perform('change', () =>
    setProjectCollectionMembership(collection.id, registrationId, belongs)
  )
}

function dissolve(): Promise<void> {
  const collection = removal.value
  if (!collection) return Promise.resolve()
  return perform(
    'change',
    () => removeProjectCollection(collection.id),
    () => {
      pendingRemoval.value = null
    }
  )
}

onMounted(refresh)
onBeforeUnmount(() => {
  disposed = true
})
</script>

<template>
  <section
    id="project-collections"
    class="panel"
    aria-labelledby="project-collections-heading"
    tabindex="-1"
  >
    <h2 id="project-collections-heading">{{ t('tauriPreview.projectCollections.title') }}</h2>
    <p class="section-description">{{ t('tauriPreview.projectCollections.intro') }}</p>
    <form data-collection-create-form class="collection-form" @submit.prevent="createCollection">
      <label class="collection-name-field" for="project-collection-new-name">
        {{ t('tauriPreview.projectCollections.name') }}
        <AInput
          id="project-collection-new-name"
          v-model:value="newName"
          :disabled="busy || !loaded"
          autocomplete="off"
        />
      </label>
      <AppButton html-type="submit" variant="primary" :disabled="busy || !loaded">
        {{ t('tauriPreview.projectCollections.create') }}
      </AppButton>
      <AppButton data-collection-refresh :disabled="busy" @click="refresh">
        {{ t('tauriPreview.projectCollections.refresh') }}
      </AppButton>
    </form>
    <div class="collection-form">
      <label>
        {{ t('tauriPreview.projectCollections.importFile') }}
        <input
          ref="importInput"
          type="file"
          accept=".json,application/json"
          :disabled="busy"
          @change="selectImport"
        />
      </label>
      <AppButton data-collection-import :disabled="busy || !selectedFile" @click="importSelected">
        {{ t('tauriPreview.projectCollections.importAction') }}
      </AppButton>
      <p v-if="imported" role="status">
        {{
          t('tauriPreview.projectCollections.imported', {
            inserted: imported.inserted,
            existing: imported.alreadyImported
          })
        }}
      </p>
    </div>
    <p v-if="pending" role="status">
      {{
        t(
          pending === 'load'
            ? 'tauriPreview.projectCollections.loading'
            : 'tauriPreview.projectCollections.saving'
        )
      }}
    </p>
    <AppAlert
      v-if="error"
      type="error"
      show-icon
      :message="t(`tauriPreview.projectCollections.${error}`)"
    />
    <p v-if="loaded && !collections.length" role="status">
      {{ t('tauriPreview.projectCollections.empty') }}
    </p>

    <div v-if="collections.length" class="collection-workspace" :aria-busy="busy">
      <nav class="collection-navigation" :aria-label="t('tauriPreview.projectCollections.list')">
        <ol class="collection-list">
          <li
            v-for="(collection, index) in collections"
            :key="collection.id"
            :data-collection-row="collection.id"
            class="collection-row"
          >
            <AppButton
              :data-collection-select="collection.id"
              class="collection-choice"
              variant="text"
              :aria-pressed="selectedId === collection.id"
              :disabled="busy"
              @click="selectCollection(collection.id)"
            >
              <span class="collection-text">{{ displayName(collection) }}</span>
              <span class="collection-count">{{
                t('tauriPreview.projectCollections.memberCount', {
                  count: collection.projectIds.length
                })
              }}</span>
            </AppButton>
            <div class="collection-actions">
              <AppButton
                :data-collection-up="collection.id"
                size="small"
                :disabled="busy || index === 0"
                @click="moveCollection(collection.id, 'up')"
              >
                {{ t('tauriPreview.projectCollections.moveUp') }}
              </AppButton>
              <AppButton
                :data-collection-down="collection.id"
                size="small"
                :disabled="busy || index === collections.length - 1"
                @click="moveCollection(collection.id, 'down')"
              >
                {{ t('tauriPreview.projectCollections.moveDown') }}
              </AppButton>
            </div>
          </li>
        </ol>
      </nav>
      <div v-if="selected" class="collection-editor">
        <h3 class="collection-text">{{ displayName(selected) }}</h3>
        <form
          data-collection-rename-form
          class="collection-form"
          @submit.prevent="renameCollection"
        >
          <label class="collection-name-field" for="project-collection-rename">
            {{ t('tauriPreview.projectCollections.name') }}
            <AInput
              id="project-collection-rename"
              v-model:value="renameDraft"
              :disabled="busy"
              autocomplete="off"
            />
          </label>
          <AppButton html-type="submit" :disabled="busy || !canRename">
            {{ t('tauriPreview.projectCollections.rename') }}
          </AppButton>
        </form>
        <h4>{{ t('tauriPreview.projectCollections.members') }}</h4>
        <p class="collection-hint">{{ t('tauriPreview.projectCollections.membersHint') }}</p>
        <ul v-if="projects.length" class="collection-members">
          <li v-for="project in projects" :key="project.id">
            <AppCheckbox
              :data-collection-project="project.id"
              :checked="selected.projectIds.includes(project.id)"
              :disabled="busy"
              @change="setMembership(project.id, $event)"
            >
              <span class="collection-project">
                <strong class="collection-text">{{ project.name }}</strong>
                <span>{{ t(`tauriPreview.projectCollections.engines.${project.engine}`) }}</span>
                <span class="collection-text collection-path">{{ project.path }}</span>
              </span>
            </AppCheckbox>
          </li>
        </ul>
        <p v-else>{{ t('tauriPreview.projectCollections.noProjects') }}</p>
        <p v-for="id in missingProjectIds" :key="id" class="collection-text collection-hint">
          {{ t('tauriPreview.projectCollections.missingProject', { id }) }}
        </p>
        <AppButton
          data-collection-dissolve
          danger
          :disabled="busy"
          @click="pendingRemoval = selected.id"
        >
          {{ t('tauriPreview.projectCollections.dissolve') }}
        </AppButton>
        <div v-if="removal" data-collection-confirm class="collection-confirm">
          <AppAlert
            type="warning"
            show-icon
            :message="
              t('tauriPreview.projectCollections.confirmDissolve', { name: displayName(removal) })
            "
          />
          <div class="collection-actions">
            <AppButton data-collection-approve danger :disabled="busy" @click="dissolve">
              {{ t('tauriPreview.projectCollections.approveDissolve') }}
            </AppButton>
            <AppButton data-collection-cancel :disabled="busy" @click="pendingRemoval = null">
              {{ t('tauriPreview.projectCollections.cancel') }}
            </AppButton>
          </div>
        </div>
      </div>
    </div>
  </section>
</template>

<style scoped src="../../panel.css"></style>
<style scoped>
.collection-form,
.collection-actions {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-2);
  align-items: center;
}

.collection-form {
  align-items: end;
  margin: var(--space-4) 0;
}

.collection-name-field {
  display: grid;
  flex: 1;
  min-width: 0;
  gap: var(--space-2);
}

.collection-workspace {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-5);
  margin-top: var(--space-5);
}

.collection-navigation {
  flex: 1 1 calc(var(--space-12) * 4);
  min-width: 0;
}

.collection-editor {
  flex: 2 1 calc(var(--space-12) * 6);
  min-width: 0;
}

.collection-list,
.collection-members {
  list-style: none;
  padding: 0;
  margin: 0 0 var(--space-4);
}

.collection-row {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-2);
  padding-bottom: var(--space-3);
  margin-bottom: var(--space-3);
  border-bottom: 1px solid var(--color-border-subtle);
}

.collection-choice {
  flex: 1;
  min-width: 0;
  height: auto;
  white-space: normal;
  justify-content: flex-start;
  text-align: start;
  padding: var(--space-3);
}

.collection-choice :deep(.app-button__label) {
  display: grid;
  min-width: 0;
  gap: var(--space-1);
}

.collection-choice[aria-pressed='true'] {
  background: var(--color-bg-soft);
  outline: 1px solid var(--color-border-focus);
}

.collection-project {
  display: grid;
  gap: var(--space-1);
  min-width: 0;
}

.collection-members > li {
  padding: var(--space-3) 0;
}

.collection-members :deep(.app-checkbox) {
  align-items: flex-start;
  max-width: 100%;
}

.collection-count,
.collection-path,
.collection-hint {
  font-size: var(--font-size-sm);
  color: var(--color-text-secondary);
  line-height: var(--line-height-normal);
}

.collection-text {
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}

.collection-confirm {
  display: grid;
  gap: var(--space-3);
  margin-top: var(--space-4);
}

h3,
h4 {
  font-size: var(--font-size-base);
  margin: 0 0 var(--space-3);
}
</style>
