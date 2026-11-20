<script setup lang="ts">
import { onBeforeUnmount, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import AppAlert from '@renderer/components/AppAlert.vue'
import AppButton from '@renderer/components/AppButton.vue'
import {
  getAgentChat,
  listAgentChats,
  type AgentChat,
  type AgentChatSummary,
  type AgentChatTurn,
  type ActorLabelReceipt
} from './commands'

const { t, locale } = useI18n()
const opened = ref(false)
const chats = ref<AgentChatSummary[]>([])
const nextOffset = ref<number | null>(null)
const listLoaded = ref(false)
const listLoading = ref(false)
const listError = ref(false)
const selectedId = ref<string | null>(null)
const chat = ref<AgentChat | null>(null)
const detailLoading = ref(false)
const detailError = ref(false)
let listRequest = 0
let detailRequest = 0
let disposed = false

function editorReceipts(
  turn: AgentChatTurn
): { approvalId: string; status: ActorLabelReceipt['status']; name: string | null }[] {
  return [
    ...(turn.actorLabelReceipts ?? []).map(({ actualLabel, ...receipt }) => ({
      ...receipt,
      name: actualLabel
    })),
    ...(turn.nodeNameReceipts ?? []).map(({ actualName, ...receipt }) => ({
      ...receipt,
      name: actualName
    }))
  ]
}

function resetDetail(): void {
  detailRequest += 1
  selectedId.value = null
  chat.value = null
  detailLoading.value = false
  detailError.value = false
}

function isCurrentList(request: number): boolean {
  return !disposed && opened.value && request === listRequest
}

function isCurrentDetail(request: number): boolean {
  return !disposed && opened.value && request === detailRequest
}

async function fetchChats(startOffset: number, append: boolean): Promise<void> {
  if (!opened.value || disposed) return
  const request = ++listRequest
  listLoading.value = true
  listError.value = false
  if (!append) resetDetail()
  try {
    const page = await listAgentChats(startOffset)
    if (!isCurrentList(request)) return
    // Offset pages can overlap as another run updates the host's ordering. Render each ID once.
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
  const request = ++detailRequest
  selectedId.value = id
  chat.value = null
  detailLoading.value = true
  detailError.value = false
  try {
    const result = await getAgentChat(id)
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

function toggle(): void {
  opened.value = !opened.value
  if (opened.value) {
    void refresh()
    return
  }
  listRequest += 1
  resetDetail()
  chats.value = []
  nextOffset.value = null
  listLoaded.value = false
  listLoading.value = false
  listError.value = false
}

function title(value: string): string {
  return value.trim() ? value : t('tauriPreview.history.untitled')
}

function dateTime(seconds: number): string {
  return new Date(seconds * 1000).toISOString()
}

function displayTime(seconds: number): string {
  return new Date(seconds * 1000).toLocaleString(locale.value)
}

onBeforeUnmount(() => {
  disposed = true
  listRequest += 1
  detailRequest += 1
})
</script>

<template>
  <section id="agent-history" class="panel" aria-labelledby="agent-history-heading" tabindex="-1">
    <h2 id="agent-history-heading">{{ t('tauriPreview.history.title') }}</h2>
    <p class="section-description">{{ t('tauriPreview.history.intro') }}</p>
    <AppButton
      data-history-toggle
      :aria-expanded="opened"
      aria-controls="agent-history-content"
      @click="toggle"
    >
      {{ t(opened ? 'tauriPreview.history.close' : 'tauriPreview.history.open') }}
    </AppButton>

    <div v-if="opened" id="agent-history-content" data-history-content class="history-content">
      <div class="history-actions">
        <AppButton data-history-refresh @click="refresh">
          {{ t('tauriPreview.history.refresh') }}
        </AppButton>
        <p v-if="listLoading" role="status">{{ t('tauriPreview.history.loadingList') }}</p>
      </div>
      <AppAlert
        v-if="listError"
        type="error"
        show-icon
        :message="t('tauriPreview.history.listError')"
      />
      <div class="history-layout">
        <nav
          class="history-list"
          :aria-label="t('tauriPreview.history.chats')"
          :aria-busy="listLoading"
        >
          <ul v-if="chats.length" class="history-rows">
            <li v-for="row in chats" :key="row.id" class="history-row">
              <AppButton
                :data-history-chat="row.id"
                class="history-chat-button"
                variant="text"
                block
                :aria-pressed="selectedId === row.id"
                @click="selectChat(row.id)"
              >
                {{ title(row.title) }}
              </AppButton>
              <div class="history-metadata">
                <span>{{ t(`tauriPreview.history.engines.${row.engine}`) }}</span>
                <span>
                  {{ t('tauriPreview.history.updatedAt') }}
                  <time :datetime="dateTime(row.updatedAt)">{{ displayTime(row.updatedAt) }}</time>
                </span>
              </div>
            </li>
          </ul>
          <p v-else-if="listLoaded && !listLoading && !listError" role="status">
            {{ t('tauriPreview.history.emptyList') }}
          </p>
          <AppButton
            v-if="nextOffset !== null"
            data-history-more
            :disabled="listLoading"
            @click="loadMore"
          >
            {{ t('tauriPreview.history.loadMore') }}
          </AppButton>
        </nav>

        <div class="history-conversation" :aria-busy="detailLoading">
          <p v-if="detailLoading" role="status">{{ t('tauriPreview.history.loadingChat') }}</p>
          <template v-else-if="detailError">
            <AppAlert type="error" show-icon :message="t('tauriPreview.history.detailError')" />
            <AppButton data-history-retry @click="retryDetail">
              {{ t('tauriPreview.history.retry') }}
            </AppButton>
          </template>
          <article v-else-if="chat" data-history-detail>
            <h3 class="history-text">{{ title(chat.title) }}</h3>
            <dl class="history-details">
              <div>
                <dt>{{ t('tauriPreview.history.engine') }}</dt>
                <dd>{{ t(`tauriPreview.history.engines.${chat.engine}`) }}</dd>
              </div>
              <div data-history-created>
                <dt>{{ t('tauriPreview.history.createdAt') }}</dt>
                <dd>
                  <time :datetime="dateTime(chat.createdAt)">{{
                    displayTime(chat.createdAt)
                  }}</time>
                </dd>
              </div>
              <div>
                <dt>{{ t('tauriPreview.history.updatedAt') }}</dt>
                <dd>
                  <time :datetime="dateTime(chat.updatedAt)">{{
                    displayTime(chat.updatedAt)
                  }}</time>
                </dd>
              </div>
            </dl>
            <ol v-if="chat.turns.length" class="history-turns">
              <li v-for="(turn, index) in chat.turns" :key="turn.id" :data-history-turn="turn.id">
                <h4>{{ t('tauriPreview.history.turnTitle', { number: index + 1 }) }}</h4>
                <p>{{ t(`tauriPreview.history.statuses.${turn.status}`) }}</p>
                <dl class="history-details">
                  <div>
                    <dt>{{ t('tauriPreview.history.createdAt') }}</dt>
                    <dd>
                      <time :datetime="dateTime(turn.createdAt)">{{
                        displayTime(turn.createdAt)
                      }}</time>
                    </dd>
                  </div>
                  <div data-history-finished>
                    <dt>{{ t('tauriPreview.history.finishedAt') }}</dt>
                    <dd>
                      <time v-if="turn.finishedAt !== null" :datetime="dateTime(turn.finishedAt)">
                        {{ displayTime(turn.finishedAt) }}
                      </time>
                      <span v-else>{{ t('tauriPreview.history.unfinished') }}</span>
                    </dd>
                  </div>
                </dl>
                <h5>{{ t('tauriPreview.history.user') }}</h5>
                <p data-history-user class="history-text">{{ turn.userText }}</p>
                <h5>{{ t('tauriPreview.history.assistant') }}</h5>
                <p data-history-assistant class="history-text">
                  {{ turn.assistantText ?? t('tauriPreview.history.noAssistantText') }}
                </p>
                <section
                  v-if="turn.actorLabelReceipts?.length || turn.nodeNameReceipts?.length"
                  data-history-receipts
                >
                  <h5>
                    {{
                      t(
                        chat.engine === 'unreal'
                          ? 'tauriPreview.history.actorReceipts.title'
                          : 'tauriPreview.history.nameReceipts.title'
                      )
                    }}
                  </h5>
                  <ul class="history-receipt-rows">
                    <li
                      v-for="receipt in editorReceipts(turn)"
                      :key="receipt.approvalId"
                      data-history-receipt
                    >
                      <p>
                        {{ t(`tauriPreview.history.actorReceipts.statuses.${receipt.status}`) }}
                      </p>
                      <dl class="history-details">
                        <div>
                          <dt>{{ t('tauriPreview.history.actorReceipts.approvalId') }}</dt>
                          <dd data-history-approval-id class="history-text">
                            {{ receipt.approvalId }}
                          </dd>
                        </div>
                        <div>
                          <dt>
                            {{
                              t(
                                chat.engine === 'unreal'
                                  ? 'tauriPreview.history.actorReceipts.actualLabel'
                                  : 'tauriPreview.history.nameReceipts.actual'
                              )
                            }}
                          </dt>
                          <dd data-history-actual-label class="history-text">
                            {{
                              receipt.name ??
                              t(
                                chat.engine === 'unreal'
                                  ? 'tauriPreview.history.actorReceipts.noActualLabel'
                                  : 'tauriPreview.history.nameReceipts.none'
                              )
                            }}
                          </dd>
                        </div>
                      </dl>
                    </li>
                  </ul>
                </section>
              </li>
            </ol>
            <p v-else role="status">{{ t('tauriPreview.history.emptyTurns') }}</p>
          </article>
          <p v-else role="status">{{ t('tauriPreview.history.selectChat') }}</p>
        </div>
      </div>
    </div>
  </section>
</template>

<style scoped src="../panel.css"></style>
<style scoped>
.history-content {
  margin-top: var(--space-4);
}

.history-actions,
.history-metadata {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-3);
}

.history-actions {
  margin-bottom: var(--space-4);
}

.history-layout {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-5);
  margin-top: var(--space-4);
}

.history-list {
  flex: 1 1 calc(var(--space-12) * 4);
  min-width: 0;
}

.history-conversation {
  flex: 2 1 calc(var(--space-12) * 6);
  min-width: 0;
}

.history-rows,
.history-turns,
.history-receipt-rows {
  list-style: none;
  padding: 0;
  margin: 0;
}

.history-row,
.history-turns > li {
  padding-bottom: var(--space-4);
  margin-bottom: var(--space-4);
  border-bottom: 1px solid var(--color-border-subtle);
}

.history-chat-button {
  height: auto;
  padding: var(--space-3);
  justify-content: flex-start;
  text-align: start;
  white-space: normal;
}

.history-chat-button :deep(.app-button__label) {
  min-width: 0;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}

.history-chat-button[aria-pressed='true'] {
  background: var(--color-bg-soft);
  outline: 1px solid var(--color-border-focus);
}

.history-metadata,
.history-details {
  color: var(--color-text-secondary);
  font-size: var(--font-size-sm);
  line-height: var(--line-height-normal);
  overflow-wrap: anywhere;
}

.history-metadata {
  padding: var(--space-2) var(--space-3) 0;
}

.history-details > div {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-2);
  margin-bottom: var(--space-1);
}

.history-details dd {
  margin: 0;
}

.history-text {
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  line-height: var(--line-height-relaxed);
}

h3,
h4,
h5 {
  margin: var(--space-3) 0;
  font-size: var(--font-size-base);
}
</style>
