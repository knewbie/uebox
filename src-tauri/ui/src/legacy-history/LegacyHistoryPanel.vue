<script setup lang="ts">
import { onBeforeUnmount, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import AppAlert from '@renderer/components/AppAlert.vue'
import AppButton from '@renderer/components/AppButton.vue'
import {
  canPreviewImage,
  getLegacyChat,
  importLegacyChatHistory,
  listLegacyChats,
  readLegacyChatImage,
  type LegacyAttachment,
  type LegacyChat,
  type LegacyChatSummary,
  type LegacyImportSummary
} from './commands'
import { ManifestReadError, readLegacyManifest } from './manifest'

interface ImagePreview {
  request: number
  loading: boolean
  error: boolean
  src?: string
}

const { t, locale } = useI18n()
const opened = ref(false)
const chats = ref<LegacyChatSummary[]>([])
const nextOffset = ref<number | null>(null)
const listLoaded = ref(false)
const listLoading = ref(false)
const listError = ref(false)
const selectedId = ref<string | null>(null)
const chat = ref<LegacyChat | null>(null)
const detailLoading = ref(false)
const detailError = ref(false)
const fileInput = ref<HTMLInputElement | null>(null)
const selectedFile = ref<File | null>(null)
const importBusy = ref(false)
const importSummary = ref<LegacyImportSummary | null>(null)
const importError = ref<'tooLarge' | 'invalidUtf8' | 'readFailed' | 'importFailed' | null>(null)
const images = ref<Record<string, ImagePreview>>({})
let openEpoch = 0
let listRequest = 0
let detailRequest = 0
let imageRequest = 0
let disposed = false

function isOpen(epoch: number): boolean {
  return !disposed && opened.value && epoch === openEpoch
}

function isCurrentList(request: number): boolean {
  return !disposed && opened.value && request === listRequest
}

function isCurrentDetail(request: number): boolean {
  return !disposed && opened.value && request === detailRequest
}

function resetDetail(): void {
  detailRequest += 1
  selectedId.value = null
  chat.value = null
  detailLoading.value = false
  detailError.value = false
  images.value = {}
}

async function fetchChats(startOffset: number, append: boolean): Promise<void> {
  if (!opened.value || disposed) return
  const request = ++listRequest
  listLoading.value = true
  listError.value = false
  if (!append) resetDetail()
  try {
    const page = await listLegacyChats(startOffset)
    if (!isCurrentList(request)) return
    const rows = append ? [...chats.value, ...page.chats] : page.chats
    chats.value = Array.from(new Map(rows.map((row) => [row.id, row])).values())
    nextOffset.value = page.nextOffset
    listLoaded.value = true
  } catch {
    if (isCurrentList(request)) listError.value = true
  } finally {
    if (isCurrentList(request)) listLoading.value = false
  }
}

function refresh(): Promise<void> {
  return fetchChats(0, false)
}

function loadMore(): Promise<void> | undefined {
  if (listLoading.value || nextOffset.value === null) return
  return fetchChats(nextOffset.value, true)
}

async function selectChat(id: string): Promise<void> {
  if (!opened.value || disposed) return
  resetDetail()
  const request = detailRequest
  selectedId.value = id
  detailLoading.value = true
  try {
    const result = await getLegacyChat(id)
    if (isCurrentDetail(request)) chat.value = result
  } catch {
    if (isCurrentDetail(request)) detailError.value = true
  } finally {
    if (isCurrentDetail(request)) detailLoading.value = false
  }
}

function retryDetail(): void {
  if (selectedId.value !== null) void selectChat(selectedId.value)
}

function chooseFile(event: Event): void {
  if (importBusy.value) return
  selectedFile.value = (event.currentTarget as HTMLInputElement).files?.[0] ?? null
  importSummary.value = null
  importError.value = null
}

function clearFile(): void {
  if (importBusy.value) return
  selectedFile.value = null
  if (fileInput.value) fileInput.value.value = ''
  importError.value = null
}

async function importArchive(): Promise<void> {
  const file = selectedFile.value
  if (!file || importBusy.value || !opened.value || disposed) return
  const epoch = openEpoch
  importBusy.value = true
  importError.value = null
  importSummary.value = null
  try {
    const manifest = await readLegacyManifest(file)
    if (!isOpen(epoch)) return
    const result = await importLegacyChatHistory(manifest)
    if (!isOpen(epoch)) return
    importSummary.value = result
    selectedFile.value = null
    if (fileInput.value) fileInput.value.value = ''
    void refresh()
  } catch (cause) {
    if (isOpen(epoch)) {
      importError.value = cause instanceof ManifestReadError ? cause.code : 'importFailed'
    }
  } finally {
    // Closing cannot cancel a host write already dispatched. Keep this guard until it settles.
    if (!disposed) importBusy.value = false
  }
}

function toggle(): void {
  opened.value = !opened.value
  if (opened.value) {
    void refresh()
    return
  }
  openEpoch += 1
  listRequest += 1
  resetDetail()
  chats.value = []
  nextOffset.value = null
  listLoaded.value = false
  listLoading.value = false
  listError.value = false
  selectedFile.value = null
  importSummary.value = null
  importError.value = null
}

function attachmentKey(message: number, attachment: number): string {
  return `${message}:${attachment}`
}

function isCurrentImage(key: string, request: number, detail: number): boolean {
  return isCurrentDetail(detail) && images.value[key]?.request === request
}

async function showImage(key: string, attachment: LegacyAttachment): Promise<void> {
  if (
    disposed ||
    !opened.value ||
    !chat.value ||
    !canPreviewImage(attachment) ||
    images.value[key]?.loading
  )
    return
  const detail = detailRequest
  const request = ++imageRequest
  images.value = { ...images.value, [key]: { request, loading: true, error: false } }
  try {
    const src = await readLegacyChatImage(attachment)
    if (isCurrentImage(key, request, detail)) {
      images.value = { ...images.value, [key]: { request, loading: false, error: false, src } }
    }
  } catch {
    if (isCurrentImage(key, request, detail)) {
      images.value = { ...images.value, [key]: { request, loading: false, error: true } }
    }
  }
}

function imageFailed(key: string, event: Event): void {
  const current = images.value[key]
  if (!current || !(event.target instanceof HTMLImageElement)) return
  if (current.src !== event.target.getAttribute('src')) return
  images.value = {
    ...images.value,
    [key]: { request: current.request, loading: false, error: true }
  }
}

function title(value: string): string {
  return value.trim() ? value : t('tauriPreview.legacyHistory.untitled')
}

function dateTime(seconds: number): string {
  return new Date(seconds * 1000).toISOString()
}

function displayTime(seconds: number): string {
  return new Date(seconds * 1000).toLocaleString(locale.value)
}

onBeforeUnmount(() => {
  disposed = true
  openEpoch += 1
  listRequest += 1
  detailRequest += 1
})
</script>

<template>
  <section id="legacy-history" class="panel" aria-labelledby="legacy-history-heading" tabindex="-1">
    <h2 id="legacy-history-heading">{{ t('tauriPreview.legacyHistory.title') }}</h2>
    <p class="section-description">{{ t('tauriPreview.legacyHistory.intro') }}</p>
    <AppButton
      data-legacy-toggle
      :aria-expanded="opened"
      aria-controls="legacy-history-content"
      @click="toggle"
    >
      {{ t(opened ? 'tauriPreview.legacyHistory.close' : 'tauriPreview.legacyHistory.open') }}
    </AppButton>
    <div v-if="opened" id="legacy-history-content" data-legacy-content class="legacy-content">
      <div class="legacy-import">
        <label for="legacy-history-file">{{ t('tauriPreview.legacyHistory.importFile') }}</label>
        <p id="legacy-history-import-hint">{{ t('tauriPreview.legacyHistory.importHint') }}</p>
        <input
          id="legacy-history-file"
          ref="fileInput"
          data-legacy-file
          type="file"
          accept=".json,application/json"
          aria-describedby="legacy-history-import-hint"
          :disabled="importBusy"
          @change="chooseFile"
          @cancel="clearFile"
        />
        <div class="legacy-actions">
          <AppButton
            data-legacy-import
            variant="primary"
            :disabled="!selectedFile || importBusy"
            @click="importArchive"
          >
            {{ t('tauriPreview.legacyHistory.confirmImport') }}
          </AppButton>
          <AppButton data-legacy-clear :disabled="!selectedFile || importBusy" @click="clearFile">
            {{ t('tauriPreview.legacyHistory.clearFile') }}
          </AppButton>
          <p v-if="importBusy" role="status">{{ t('tauriPreview.legacyHistory.importing') }}</p>
        </div>
        <AppAlert
          v-if="importError"
          type="error"
          show-icon
          :message="t(`tauriPreview.legacyHistory.errors.${importError}`)"
        />
        <p v-if="importSummary" data-legacy-imported role="status">
          {{ t('tauriPreview.legacyHistory.imported', importSummary) }}
        </p>
      </div>

      <div class="legacy-actions">
        <AppButton data-legacy-refresh @click="refresh">{{
          t('tauriPreview.legacyHistory.refresh')
        }}</AppButton>
        <p v-if="listLoading" role="status">{{ t('tauriPreview.legacyHistory.loadingList') }}</p>
      </div>
      <AppAlert
        v-if="listError"
        type="error"
        show-icon
        :message="t('tauriPreview.legacyHistory.listError')"
      />
      <div class="legacy-layout">
        <nav
          class="legacy-list"
          :aria-label="t('tauriPreview.legacyHistory.chats')"
          :aria-busy="listLoading"
        >
          <ul v-if="chats.length" class="legacy-rows">
            <li v-for="row in chats" :key="row.id" class="legacy-row">
              <AppButton
                :data-legacy-chat="row.id"
                class="legacy-chat-button"
                variant="text"
                block
                :aria-pressed="selectedId === row.id"
                @click="selectChat(row.id)"
                >{{ title(row.title) }}</AppButton
              >
              <p class="legacy-metadata">
                {{ t('tauriPreview.legacyHistory.updatedAt') }}
                <time :datetime="dateTime(row.updatedAt)">{{ displayTime(row.updatedAt) }}</time>
              </p>
            </li>
          </ul>
          <p v-else-if="listLoaded && !listLoading && !listError" role="status">
            {{ t('tauriPreview.legacyHistory.emptyList') }}
          </p>
          <AppButton
            v-if="nextOffset !== null"
            data-legacy-more
            :disabled="listLoading"
            @click="loadMore"
          >
            {{ t('tauriPreview.legacyHistory.loadMore') }}
          </AppButton>
        </nav>
        <div class="legacy-conversation" :aria-busy="detailLoading">
          <p v-if="detailLoading" role="status">
            {{ t('tauriPreview.legacyHistory.loadingChat') }}
          </p>
          <template v-else-if="detailError">
            <AppAlert
              type="error"
              show-icon
              :message="t('tauriPreview.legacyHistory.detailError')"
            />
            <AppButton data-legacy-retry @click="retryDetail">{{
              t('tauriPreview.legacyHistory.retry')
            }}</AppButton>
          </template>
          <article v-else-if="chat" data-legacy-detail>
            <h3 class="legacy-text">{{ title(chat.title) }}</h3>
            <dl class="legacy-dates">
              <div data-legacy-created>
                <dt>{{ t('tauriPreview.legacyHistory.createdAt') }}</dt>
                <dd>
                  <time :datetime="dateTime(chat.createdAt)">{{
                    displayTime(chat.createdAt)
                  }}</time>
                </dd>
              </div>
              <div>
                <dt>{{ t('tauriPreview.legacyHistory.updatedAt') }}</dt>
                <dd>
                  <time :datetime="dateTime(chat.updatedAt)">{{
                    displayTime(chat.updatedAt)
                  }}</time>
                </dd>
              </div>
            </dl>
            <ol v-if="chat.messages.length" class="legacy-messages">
              <li
                v-for="(message, messageIndex) in chat.messages"
                :key="messageIndex"
                class="legacy-message"
              >
                <h4>{{ t(`tauriPreview.legacyHistory.roles.${message.role}`) }}</h4>
                <p>{{ t(`tauriPreview.legacyHistory.statuses.${message.status}`) }}</p>
                <p data-legacy-message-text class="legacy-text">{{ message.text }}</p>
                <section v-if="message.attachments?.length">
                  <h5>{{ t('tauriPreview.legacyHistory.attachments.title') }}</h5>
                  <ul class="legacy-attachments">
                    <li
                      v-for="(attachment, attachmentIndex) in message.attachments"
                      :key="attachmentIndex"
                      class="legacy-attachment"
                    >
                      <p class="legacy-text">
                        {{
                          t('tauriPreview.legacyHistory.attachments.kind', {
                            kind: attachment.kind
                          })
                        }}
                      </p>
                      <template v-if="canPreviewImage(attachment)">
                        <img
                          v-if="images[attachmentKey(messageIndex, attachmentIndex)]?.src"
                          :src="images[attachmentKey(messageIndex, attachmentIndex)]?.src"
                          :alt="t('tauriPreview.legacyHistory.attachments.imageAlt')"
                          referrerpolicy="no-referrer"
                          @error="imageFailed(attachmentKey(messageIndex, attachmentIndex), $event)"
                        />
                        <template v-else>
                          <AppAlert
                            v-if="images[attachmentKey(messageIndex, attachmentIndex)]?.error"
                            type="error"
                            show-icon
                            :message="t('tauriPreview.legacyHistory.attachments.imageError')"
                          />
                          <AppButton
                            data-legacy-image
                            :disabled="
                              images[attachmentKey(messageIndex, attachmentIndex)]?.loading
                            "
                            @click="
                              showImage(attachmentKey(messageIndex, attachmentIndex), attachment)
                            "
                          >
                            {{
                              t(
                                images[attachmentKey(messageIndex, attachmentIndex)]?.error
                                  ? 'tauriPreview.legacyHistory.attachments.retryImage'
                                  : 'tauriPreview.legacyHistory.attachments.viewImage'
                              )
                            }}
                          </AppButton>
                          <p
                            v-if="images[attachmentKey(messageIndex, attachmentIndex)]?.loading"
                            role="status"
                          >
                            {{ t('tauriPreview.legacyHistory.attachments.loadingImage') }}
                          </p>
                        </template>
                      </template>
                      <p v-else data-legacy-unavailable>
                        {{ t('tauriPreview.legacyHistory.attachments.unavailable') }}
                      </p>
                    </li>
                  </ul>
                </section>
              </li>
            </ol>
            <p v-else role="status">{{ t('tauriPreview.legacyHistory.emptyMessages') }}</p>
          </article>
          <p v-else role="status">{{ t('tauriPreview.legacyHistory.selectChat') }}</p>
        </div>
      </div>
    </div>
  </section>
</template>

<style scoped src="../panel.css"></style>
<style scoped src="./legacy-history.css"></style>
