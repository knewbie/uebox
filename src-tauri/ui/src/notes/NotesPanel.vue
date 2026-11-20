<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, shallowRef } from 'vue'
import { useI18n } from 'vue-i18n'
import AInput from 'ant-design-vue/es/input/Input'
import AppAlert from '@renderer/components/AppAlert.vue'
import AppButton from '@renderer/components/AppButton.vue'
import LegacyNotesImport from './LegacyNotesImport.vue'
import {
  getNote,
  listNotebooks,
  listNotes,
  removeNote,
  removeNotebook,
  saveNote,
  saveNotebook,
  type Note,
  type Notebook,
  type NoteSummary
} from './commands'

type DeleteTarget = { kind: 'note' | 'notebook'; id: string; title: string }
const { t } = useI18n()
const notebooks = ref<Notebook[]>([])
const notebooksNext = ref<string | null>(null)
const notebooksLoaded = ref(false)
const notebooksLoading = ref(false)
const notebooksError = ref(false)
const scope = ref<Notebook | null>(null)
const notes = ref<NoteSummary[]>([])
const notesNext = ref<string | null>(null)
const notesLoaded = ref(false)
const notesLoading = ref(false)
const notesError = ref(false)
const selectedId = ref<string | null>(null)
const note = ref<Note | null>(null)
const noteLoading = ref(false)
const noteError = ref(false)
const mode = ref<'view' | 'edit' | 'new'>('view')
const draftTitle = ref('')
const draftContent = ref('')
const newNotebookTitle = ref('')
const notebookTitle = ref('')
const writing = ref(false)
const writeError = ref<'saveError' | 'deleteError' | null>(null)
const deleting = ref<DeleteTarget | null>(null)
const pendingNavigation = shallowRef<(() => void) | null>(null)
const locked = computed(
  () => writing.value || deleting.value !== null || pendingNavigation.value !== null
)
const dirty = computed(
  () =>
    mode.value !== 'view' &&
    (draftTitle.value !== (mode.value === 'edit' ? (note.value?.title ?? '') : '') ||
      draftContent.value !== (mode.value === 'edit' ? (note.value?.content ?? '') : ''))
)
let booksRequest = 0
let notesRequest = 0
let detailRequest = 0
let disposed = false

function mergeById<T extends { id: string }>(before: readonly T[], incoming: readonly T[]): T[] {
  return Array.from(new Map([...before, ...incoming].map((item) => [item.id, item])).values())
}

function displayTitle(title: string): string {
  return title.trim() ? title : t('tauriPreview.notes.untitled')
}

async function fetchBooks(append = false): Promise<void> {
  if (disposed || (append && (notebooksLoading.value || notebooksNext.value === null))) return
  const request = ++booksRequest
  const afterId = append ? notebooksNext.value : null
  notebooksLoading.value = true
  notebooksError.value = false
  try {
    const page = await listNotebooks(afterId)
    if (disposed || request !== booksRequest) return
    notebooks.value = mergeById(append ? notebooks.value : [], page.items)
    notebooksNext.value = page.nextId
    notebooksLoaded.value = true
  } catch {
    if (!disposed && request === booksRequest) notebooksError.value = true
  } finally {
    if (!disposed && request === booksRequest) notebooksLoading.value = false
  }
}

async function fetchNotes(append = false): Promise<void> {
  if (disposed || (append && (notesLoading.value || notesNext.value === null))) return
  const request = ++notesRequest
  const notebookId = scope.value?.id ?? null
  const afterId = append ? notesNext.value : null
  notesLoading.value = true
  notesError.value = false
  try {
    const page = await listNotes(notebookId, afterId)
    if (disposed || request !== notesRequest) return
    notes.value = mergeById(append ? notes.value : [], page.items)
    notesNext.value = page.nextId
    notesLoaded.value = true
  } catch {
    if (!disposed && request === notesRequest) notesError.value = true
  } finally {
    if (!disposed && request === notesRequest) notesLoading.value = false
  }
}

function resetNote(): void {
  detailRequest += 1
  selectedId.value = null
  note.value = null
  noteLoading.value = false
  noteError.value = false
  mode.value = 'view'
  draftTitle.value = ''
  draftContent.value = ''
  writeError.value = null
}

function navigate(action: () => void): void {
  if (locked.value || disposed) return
  if (dirty.value) pendingNavigation.value = action
  else action()
}

function discardAndContinue(): void {
  const action = pendingNavigation.value
  pendingNavigation.value = null
  action?.()
}

function selectScope(notebook: Notebook | null): void {
  if ((scope.value?.id ?? null) === (notebook?.id ?? null)) return
  navigate(() => {
    scope.value = notebook
    notebookTitle.value = notebook?.title ?? ''
    notes.value = []
    notesNext.value = null
    notesLoaded.value = false
    resetNote()
    void fetchNotes()
  })
}

function refreshNotes(): void {
  navigate(() => {
    resetNote()
    void fetchNotes()
  })
}

async function openNote(id: string): Promise<void> {
  resetNote()
  selectedId.value = id
  const request = detailRequest
  const notebookId = scope.value?.id ?? null
  noteLoading.value = true
  try {
    const result = await getNote(id)
    if (result.notebookId !== notebookId) throw new Error('notes_unavailable')
    if (!disposed && request === detailRequest) note.value = result
  } catch {
    if (!disposed && request === detailRequest) noteError.value = true
  } finally {
    if (!disposed && request === detailRequest) noteLoading.value = false
  }
}

function selectNote(id: string): void {
  navigate(() => {
    void openNote(id)
  })
}

function retryNote(): void {
  if (selectedId.value !== null) selectNote(selectedId.value)
}

function newNote(): void {
  navigate(() => {
    resetNote()
    mode.value = 'new'
  })
}

function editNote(): void {
  if (!note.value || locked.value) return
  draftTitle.value = note.value.title
  draftContent.value = note.value.content
  mode.value = 'edit'
}

function cancelEdit(): void {
  mode.value = 'view'
  draftTitle.value = ''
  draftContent.value = ''
  writeError.value = null
}

function invalidateBooks(): void {
  booksRequest += 1
  notebooksLoading.value = false
}

function invalidateNotes(): void {
  notesRequest += 1
  notesLoading.value = false
}

async function write(
  kind: 'saveError' | 'deleteError',
  action: () => Promise<void>
): Promise<void> {
  if (writing.value || disposed) return
  writing.value = true
  writeError.value = null
  try {
    await action()
  } catch {
    if (!disposed) writeError.value = kind
  } finally {
    if (!disposed) writing.value = false
  }
}

function saveBook(id: string | null, title: string): Promise<void> {
  if (locked.value || !title.trim()) return Promise.resolve()
  return write('saveError', async () => {
    invalidateBooks()
    const saved = await saveNotebook({ id, title: title.trim() })
    if (disposed) return
    notebooks.value = mergeById(notebooks.value, [saved])
    notebooksLoaded.value = true
    notebooksError.value = false
    if (id === null) newNotebookTitle.value = ''
    if (scope.value?.id === saved.id) {
      scope.value = saved
      notebookTitle.value = saved.title
    }
  })
}

function saveCurrentNote(): Promise<void> {
  if (locked.value || mode.value === 'view' || !draftTitle.value.trim()) return Promise.resolve()
  const input = {
    id: mode.value === 'edit' ? (note.value?.id ?? null) : null,
    notebookId:
      mode.value === 'edit' ? (note.value?.notebookId ?? null) : (scope.value?.id ?? null),
    title: draftTitle.value.trim(),
    content: draftContent.value
  }
  return write('saveError', async () => {
    invalidateNotes()
    const saved = await saveNote(input)
    if (disposed) return
    note.value = saved
    selectedId.value = saved.id
    notes.value = mergeById(notes.value, [
      { id: saved.id, title: saved.title, notebookId: saved.notebookId }
    ])
    notesLoaded.value = true
    notesError.value = false
    mode.value = 'view'
    draftTitle.value = saved.title
    draftContent.value = saved.content
  })
}

function askDelete(kind: 'note' | 'notebook'): void {
  const target = kind === 'note' ? note.value : scope.value
  if (target && !locked.value) deleting.value = { kind, id: target.id, title: target.title }
}

function approveDelete(): Promise<void> {
  const target = deleting.value
  if (!target) return Promise.resolve()
  return write('deleteError', async () => {
    invalidateNotes()
    detailRequest += 1
    noteLoading.value = false
    if (target.kind === 'notebook') invalidateBooks()
    const removed =
      target.kind === 'notebook' ? await removeNotebook(target.id) : await removeNote(target.id)
    if (disposed) return
    if (!removed) throw new Error('notes_unavailable')
    deleting.value = null
    if (target.kind === 'note') {
      notes.value = notes.value.filter((item) => item.id !== target.id)
      if (selectedId.value === target.id) resetNote()
    } else {
      notebooks.value = notebooks.value.filter((item) => item.id !== target.id)
      if (scope.value?.id === target.id) {
        scope.value = null
        notebookTitle.value = ''
        notes.value = []
        notesNext.value = null
        notesLoaded.value = false
        resetNote()
        void fetchNotes()
      }
    }
  })
}

function refreshLists(): void {
  void fetchBooks()
  void fetchNotes()
}

onMounted(refreshLists)
onBeforeUnmount(() => {
  disposed = true
  booksRequest += 1
  notesRequest += 1
  detailRequest += 1
})
</script>

<template>
  <section id="notes" class="panel" aria-labelledby="notes-heading" tabindex="-1">
    <h2 id="notes-heading">{{ t('tauriPreview.notes.title') }}</h2>
    <p class="section-description">{{ t('tauriPreview.notes.intro') }}</p>
    <LegacyNotesImport @imported="refreshLists" />
    <p v-if="writing" role="status">{{ t('tauriPreview.notes.saving') }}</p>
    <AppAlert
      v-if="writeError"
      data-notes-write-error
      type="error"
      show-icon
      :message="t(`tauriPreview.notes.${writeError}`)"
    />
    <div v-if="pendingNavigation" data-notes-discard-confirm class="notes-confirm">
      <AppAlert type="warning" show-icon :message="t('tauriPreview.notes.unsaved')" />
      <div class="notes-actions">
        <AppButton data-notes-discard-approve @click="discardAndContinue">{{
          t('tauriPreview.notes.discard')
        }}</AppButton>
        <AppButton data-notes-discard-cancel @click="pendingNavigation = null">{{
          t('tauriPreview.notes.cancel')
        }}</AppButton>
      </div>
    </div>
    <div v-if="deleting" data-notes-delete-confirm class="notes-confirm">
      <AppAlert
        type="warning"
        show-icon
        :message="
          t(
            deleting.kind === 'notebook'
              ? 'tauriPreview.notes.confirmDeleteNotebook'
              : 'tauriPreview.notes.confirmDeleteNote',
            { title: displayTitle(deleting.title) }
          )
        "
      />
      <div class="notes-actions">
        <AppButton data-notes-delete-approve danger :disabled="writing" @click="approveDelete">{{
          t('tauriPreview.notes.approveDelete')
        }}</AppButton>
        <AppButton data-notes-delete-cancel :disabled="writing" @click="deleting = null">{{
          t('tauriPreview.notes.cancel')
        }}</AppButton>
      </div>
    </div>
    <div class="notes-workspace">
      <nav class="notes-books" :aria-label="t('tauriPreview.notes.notebooks')">
        <h3>{{ t('tauriPreview.notes.notebooks') }}</h3>
        <AppButton
          data-notes-standalone
          block
          :aria-pressed="scope === null"
          :disabled="locked"
          @click="selectScope(null)"
          >{{ t('tauriPreview.notes.standalone') }}</AppButton
        >
        <form
          data-notebook-create-form
          class="notes-form"
          @submit.prevent="saveBook(null, newNotebookTitle)"
        >
          <label for="notes-new-notebook">{{ t('tauriPreview.notes.newNotebookTitle') }}</label>
          <AInput
            id="notes-new-notebook"
            v-model:value="newNotebookTitle"
            :disabled="locked || !notebooksLoaded"
            required
          />
          <AppButton
            html-type="submit"
            :disabled="locked || !notebooksLoaded || !newNotebookTitle.trim()"
            >{{ t('tauriPreview.notes.createNotebook') }}</AppButton
          >
        </form>
        <AppButton :disabled="locked || notebooksLoading" @click="fetchBooks()">{{
          t('tauriPreview.notes.refresh')
        }}</AppButton>
        <p v-if="notebooksLoading" role="status">{{ t('tauriPreview.notes.loading') }}</p>
        <AppAlert
          v-if="notebooksError"
          type="error"
          :message="t('tauriPreview.notes.loadNotebooksError')"
        />
        <ul v-if="notebooks.length" class="notes-list">
          <li v-for="item in notebooks" :key="item.id">
            <AppButton
              :data-notebook-select="item.id"
              class="notes-choice"
              variant="text"
              block
              :aria-pressed="scope?.id === item.id"
              :disabled="locked"
              @click="selectScope(item)"
              >{{ displayTitle(item.title) }}</AppButton
            >
          </li>
        </ul>
        <p v-else-if="notebooksLoaded && !notebooksLoading && !notebooksError">
          {{ t('tauriPreview.notes.emptyNotebooks') }}
        </p>
        <AppButton
          v-if="notebooksNext !== null"
          data-notebook-more
          :disabled="locked || notebooksLoading"
          @click="fetchBooks(true)"
          >{{ t('tauriPreview.notes.more') }}</AppButton
        >
      </nav>
      <section class="notes-index" aria-labelledby="notes-scope-heading">
        <h3 id="notes-scope-heading" class="notes-text">
          {{ scope ? displayTitle(scope.title) : t('tauriPreview.notes.standalone') }}
        </h3>
        <form
          v-if="scope"
          data-notebook-save-form
          class="notes-form"
          @submit.prevent="saveBook(scope.id, notebookTitle)"
        >
          <label for="notes-notebook-title">{{ t('tauriPreview.notes.notebookTitle') }}</label>
          <AInput
            id="notes-notebook-title"
            v-model:value="notebookTitle"
            :disabled="locked"
            required
          />
          <AppButton
            html-type="submit"
            :disabled="locked || !notebookTitle.trim() || notebookTitle === scope.title"
            >{{ t('tauriPreview.notes.saveNotebook') }}</AppButton
          >
        </form>
        <div class="notes-actions">
          <AppButton data-note-new :disabled="locked || !notesLoaded" @click="newNote">{{
            t('tauriPreview.notes.newNote')
          }}</AppButton>
          <AppButton :disabled="locked || notesLoading" @click="refreshNotes">{{
            t('tauriPreview.notes.refresh')
          }}</AppButton>
          <AppButton
            v-if="scope"
            data-notebook-remove
            danger
            :disabled="locked"
            @click="askDelete('notebook')"
            >{{ t('tauriPreview.notes.deleteNotebook') }}</AppButton
          >
        </div>
        <p v-if="notesLoading" role="status">{{ t('tauriPreview.notes.loading') }}</p>
        <AppAlert
          v-if="notesError"
          type="error"
          :message="t('tauriPreview.notes.loadNotesError')"
        />
        <ul v-if="notes.length" class="notes-list">
          <li v-for="item in notes" :key="item.id">
            <AppButton
              :data-note-select="item.id"
              class="notes-choice"
              variant="text"
              block
              :aria-pressed="selectedId === item.id"
              :disabled="locked"
              @click="selectNote(item.id)"
              >{{ displayTitle(item.title) }}</AppButton
            >
          </li>
        </ul>
        <p v-else-if="notesLoaded && !notesLoading && !notesError">
          {{ t('tauriPreview.notes.emptyNotes') }}
        </p>
        <AppButton
          v-if="notesNext !== null"
          data-notes-more
          :disabled="locked || notesLoading"
          @click="fetchNotes(true)"
          >{{ t('tauriPreview.notes.more') }}</AppButton
        >
      </section>
      <section class="notes-document" :aria-busy="noteLoading || writing">
        <p v-if="noteLoading" role="status">{{ t('tauriPreview.notes.loading') }}</p>
        <template v-else-if="noteError">
          <AppAlert type="error" :message="t('tauriPreview.notes.loadNoteError')" />
          <AppButton :disabled="locked" @click="retryNote">{{
            t('tauriPreview.notes.retry')
          }}</AppButton>
        </template>
        <form
          v-else-if="mode !== 'view'"
          data-note-editor
          class="notes-form"
          @submit.prevent="saveCurrentNote"
        >
          <label for="notes-note-title">{{ t('tauriPreview.notes.noteTitle') }}</label>
          <AInput id="notes-note-title" v-model:value="draftTitle" :disabled="locked" required />
          <label for="notes-note-content">{{ t('tauriPreview.notes.content') }}</label>
          <textarea
            id="notes-note-content"
            v-model="draftContent"
            rows="12"
            :disabled="locked"
            spellcheck="false"
          ></textarea>
          <div class="notes-actions">
            <AppButton
              html-type="submit"
              variant="primary"
              :disabled="locked || !draftTitle.trim()"
              >{{ t('tauriPreview.notes.saveNote') }}</AppButton
            >
            <AppButton :disabled="locked" @click="cancelEdit">{{
              t('tauriPreview.notes.cancelEdit')
            }}</AppButton>
          </div>
        </form>
        <article v-else-if="note">
          <h3 class="notes-text">{{ displayTitle(note.title) }}</h3>
          <div class="notes-actions">
            <AppButton data-note-edit :disabled="locked" @click="editNote">{{
              t('tauriPreview.notes.editNote')
            }}</AppButton>
            <AppButton data-note-remove danger :disabled="locked" @click="askDelete('note')">{{
              t('tauriPreview.notes.deleteNote')
            }}</AppButton>
          </div>
          <pre data-note-content class="notes-text notes-content">{{ note.content }}</pre>
        </article>
        <p v-else>{{ t('tauriPreview.notes.noSelection') }}</p>
      </section>
    </div>
  </section>
</template>

<style scoped src="../panel.css"></style>
<style scoped src="./notes.css"></style>
