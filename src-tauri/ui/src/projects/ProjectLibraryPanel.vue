<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import ProjectPropertiesPanel from './ProjectPropertiesPanel.vue'
import AInput from 'ant-design-vue/es/input/Input'
import AppAlert from '@renderer/components/AppAlert.vue'
import AppButton from '@renderer/components/AppButton.vue'
import ProjectCollectionsPanel from './collections/ProjectCollectionsPanel.vue'
import {
  importRegisteredProjects,
  listRegisteredProjects,
  openRegisteredProjectFolder,
  registerProject,
  removeRegisteredProject,
  type ProjectEngine,
  type ProjectImportSummary,
  type RegisteredProject
} from './commands'

const { t } = useI18n()
defineProps<{ collectionsEnabled?: boolean }>()
const engines: readonly ProjectEngine[] = ['unreal', 'unity', 'godot']
const engine = ref<ProjectEngine | ''>('')
const name = ref('')
const path = ref('')
const projects = ref<RegisteredProject[]>([])
const editedProject = ref<string | null>(null)
const pending = ref<'list' | 'register' | 'remove' | 'import' | 'open' | null>(null)
const listError = ref<string | null>(null)
const writeError = ref<string | null>(null)
const importInput = ref<HTMLInputElement | null>(null)
const importFile = ref<File | null>(null)
const importSummary = ref<ProjectImportSummary | null>(null)
const maxImportBytes = 1_048_576
const canRegister = computed(
  () => !pending.value && Boolean(engine.value && name.value.trim() && path.value.trim())
)
const knownErrors = new Set([
  'project_library_invalid',
  'project_library_locked',
  'project_library_unavailable',
  'project_library_recovery_required',
  'project_invalid',
  'project_missing',
  'project_offline',
  'project_folder_unavailable',
  'project_duplicate',
  'project_import_invalid',
  'project_import_version',
  'project_import_platform',
  'project_import_size'
])

function errorCode(cause: unknown): string {
  return typeof cause === 'string' && knownErrors.has(cause) ? cause : 'generic'
}

async function perform(
  operation: NonNullable<typeof pending.value>,
  action: () => Promise<void>
): Promise<void> {
  // Serialize the initial read, refreshes and mutations so no list response can replace a write.
  if (pending.value) return
  pending.value = operation
  const error = operation === 'list' ? listError : writeError
  error.value = null
  try {
    await action()
  } catch (cause) {
    error.value = errorCode(cause)
  } finally {
    pending.value = null
  }
}

function refresh(): Promise<void> {
  return perform('list', async () => {
    projects.value = await listRegisteredProjects()
  })
}

function addProject(): void {
  const selectedEngine = engine.value
  if (!canRegister.value || selectedEngine === '') return
  void perform('register', async () => {
    const record = await registerProject(selectedEngine, name.value, path.value)
    projects.value.push(record)
    engine.value = ''
    name.value = ''
    path.value = ''
  })
}

function openFolder(id: string): Promise<void> {
  return perform('open', () => openRegisteredProjectFolder(id))
}

function removeProject(id: string): Promise<void> {
  return perform('remove', async () => {
    await removeRegisteredProject(id)
    // Both resolved results mean this registration is absent from the host.
    projects.value = projects.value.filter((project) => project.id !== id)
  })
}

function selectImportFile(event: Event): void {
  importFile.value = (event.currentTarget as HTMLInputElement).files?.[0] ?? null
  if (importSummary.value) importSummary.value = null
  if (writeError.value?.startsWith('project_import_')) writeError.value = null
}

function importProjects(): Promise<void> {
  const file = importFile.value
  if (!file) return Promise.resolve()
  return perform('import', async () => {
    if (file.size > maxImportBytes) throw 'project_import_size'
    let manifest: string
    try {
      manifest = new TextDecoder('utf-8', { fatal: true }).decode(await file.arrayBuffer())
    } catch {
      throw 'project_import_invalid'
    }
    importSummary.value = await importRegisteredProjects(manifest)
    importFile.value = null
    if (importInput.value) importInput.value.value = ''
    listError.value = null
    try {
      projects.value = await listRegisteredProjects()
    } catch (cause) {
      listError.value = errorCode(cause)
    }
  })
}

onMounted(refresh)
</script>

<template>
  <section
    id="project-registration"
    class="panel"
    aria-labelledby="project-registration-heading"
    tabindex="-1"
  >
    <h2 id="project-registration-heading">{{ t('tauriPreview.projectLibrary.title') }}</h2>
    <p class="section-description">{{ t('tauriPreview.projectLibrary.intro') }}</p>

    <form class="project-form" @submit.prevent="addProject">
      <label for="project-engine">{{ t('tauriPreview.projectLibrary.engine') }}</label>
      <select
        id="project-engine"
        v-model="engine"
        name="project-engine"
        :disabled="pending !== null"
      >
        <option value="" disabled>{{ t('tauriPreview.projectLibrary.chooseEngine') }}</option>
        <option v-for="choice in engines" :key="choice" :value="choice">
          {{ t(`tauriPreview.projectLibrary.engines.${choice}`) }}
        </option>
      </select>
      <label for="project-name">{{ t('tauriPreview.projectLibrary.name') }}</label>
      <AInput
        id="project-name"
        v-model:value="name"
        name="project-name"
        autocomplete="off"
        :disabled="pending !== null"
      />
      <label for="project-path">{{ t('tauriPreview.projectLibrary.path') }}</label>
      <AInput
        id="project-path"
        v-model:value="path"
        name="project-path"
        autocomplete="off"
        :disabled="pending !== null"
      />
      <AppButton
        html-type="submit"
        variant="primary"
        :disabled="!canRegister"
        :loading="pending === 'register'"
      >
        {{ t('tauriPreview.projectLibrary.add') }}
      </AppButton>
    </form>
    <div class="project-transfer">
      <label for="project-import-file">{{ t('tauriPreview.projectLibrary.importFile') }}</label>
      <input
        id="project-import-file"
        ref="importInput"
        type="file"
        accept=".json,application/json"
        :disabled="pending !== null"
        @change="selectImportFile"
      />
      <AppButton
        data-project-import
        :disabled="!importFile || pending !== null"
        :loading="pending === 'import'"
        @click="importProjects"
        >{{ t('tauriPreview.projectLibrary.importAction') }}</AppButton
      >
      <p v-if="importSummary" role="status">
        {{ t('tauriPreview.projectLibrary.imported', importSummary) }}
      </p>
    </div>
    <AppAlert
      v-if="writeError"
      type="error"
      show-icon
      :message="t(`tauriPreview.projectLibrary.errors.${writeError}`)"
    />

    <div class="project-list-actions">
      <AppButton data-project-refresh :disabled="pending !== null" @click="refresh">
        {{ t('tauriPreview.projectLibrary.refresh') }}
      </AppButton>
      <p v-if="pending === 'list'" role="status">{{ t('tauriPreview.projectLibrary.loading') }}</p>
    </div>
    <AppAlert
      v-if="listError"
      type="error"
      show-icon
      :message="t('tauriPreview.projectLibrary.listError')"
      :description="t(`tauriPreview.projectLibrary.errors.${listError}`)"
    />
    <ul v-if="projects.length" class="project-list">
      <li v-for="project in projects" :key="project.id" class="project-row">
        <div class="project-details">
          <strong>{{ project.name }}</strong>
          <span>{{ t(`tauriPreview.projectLibrary.engines.${project.engine}`) }}</span>
          <span v-if="project.pinned">{{
            t('tauriPreview.projectLibrary.properties.pinned')
          }}</span>
          <code>{{ project.path }}</code>
        </div>
        <AppButton
          data-project-folder
          :disabled="pending !== null"
          :aria-label="t('tauriPreview.projectLibrary.openNamed', { name: project.name })"
          @click="openFolder(project.id)"
        >
          {{ t('tauriPreview.projectLibrary.openFolder') }}
        </AppButton>
        <AppButton
          data-project-properties
          :disabled="pending !== null"
          :aria-expanded="editedProject === project.id"
          @click="editedProject = editedProject === project.id ? null : project.id"
          >{{ t('tauriPreview.projectLibrary.properties.open') }}</AppButton
        >
        <AppButton
          data-project-remove
          :disabled="pending !== null"
          :aria-label="t('tauriPreview.projectLibrary.removeNamed', { name: project.name })"
          @click="removeProject(project.id)"
        >
          {{ t('tauriPreview.projectLibrary.remove') }}
        </AppButton>
        <ProjectPropertiesPanel
          v-if="editedProject === project.id"
          :key="project.id"
          :project="project"
          class="project-properties-row"
          @updated="refresh"
          @close="editedProject = null"
        />
      </li>
    </ul>
    <p v-else-if="pending !== 'list' && !listError">{{ t('tauriPreview.projectLibrary.empty') }}</p>
    <ProjectCollectionsPanel v-if="collectionsEnabled" :projects="projects" />
  </section>
</template>

<style scoped src="../panel.css"></style>
<style scoped>
.project-transfer {
  display: grid;
  justify-items: start;
  gap: var(--space-2);
  margin-bottom: var(--space-4);
}
.project-transfer input {
  max-width: 100%;
}
.project-form {
  display: grid;
  gap: var(--space-3);
  margin-bottom: var(--space-4);
}
.project-form select,
.project-form :deep(.ant-input) {
  color: var(--color-text-primary);
  background: var(--color-bg-page);
  border: 1px solid var(--color-border);
  border-radius: var(--radius-md);
}
.project-form select {
  font: inherit;
  padding: var(--space-1) var(--space-2);
}
.project-form select:disabled,
.project-form :deep(.ant-input:disabled) {
  color: var(--color-text-muted);
  background: var(--color-bg-surface);
}
.project-form select:focus-visible,
.project-form :deep(.ant-input:focus-visible) {
  outline: 2px solid var(--color-border-focus);
  outline-offset: 2px;
}
.project-form .app-button {
  justify-self: start;
}
.project-list-actions {
  display: flex;
  align-items: center;
  gap: var(--space-3);
  margin: var(--space-4) 0;
}
.project-list-actions p {
  margin: 0;
}
.project-list {
  list-style: none;
  padding: 0;
  margin: 0;
}
.project-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-3);
  padding: var(--space-3) 0;
  border-top: 1px solid var(--color-border-subtle);
}
.project-details {
  display: grid;
  min-width: 0;
  gap: var(--space-1);
  overflow-wrap: anywhere;
}
.project-details span,
.project-details code {
  color: var(--color-text-secondary);
  font-size: var(--font-size-sm);
}
.project-properties-row {
  grid-column: 1 / -1;
}
</style>
