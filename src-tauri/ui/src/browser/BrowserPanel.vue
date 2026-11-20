<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import AppAlert from '@renderer/components/AppAlert.vue'
import AppButton from '@renderer/components/AppButton.vue'
import {
  browserStatus,
  controlBrowser,
  openBrowser,
  saveBrowserLink,
  type BrowserAction,
  type BrowserStatus
} from './commands'

const { t } = useI18n()
const status = ref<BrowserStatus | null>(null)
const address = ref('')
const downloadAddress = ref('')
const downloading = ref(false)
const downloadError = ref<string | null>(null)
const downloadSaved = ref(false)
const downloadErrors = new Set([
  'browser_download_failed',
  'browser_download_busy',
  'browser_download_size',
  'browser_download_destination',
  'browser_download_origin',
  'browser_invalid_session'
])
const pending = ref(false)
const statusError = ref(false)
const actionError = ref<string | null>(null)
const errorCodes = new Set([
  'browser_invalid_url',
  'browser_already_open',
  'browser_invalid_session',
  'browser_no_history',
  'browser_history_unavailable'
])
const canControl = computed(() => status.value?.open && !status.value.opening && !pending.value)
const phase = computed(() => {
  if (status.value?.opening) return 'opening'
  if (!status.value) return 'checking'
  if (!status.value.open) return 'closed'
  return status.value.loading ? 'loading' : 'opened'
})
let generation = 0
let reading = false
let disposed = false
let timer: ReturnType<typeof setInterval> | undefined

async function refresh(): Promise<void> {
  if (disposed || reading || pending.value) return
  const read = generation
  reading = true
  try {
    const reply = await browserStatus()
    if (!disposed && read === generation) {
      status.value = reply
      statusError.value = false
    }
  } catch {
    if (!disposed && read === generation) statusError.value = true
  } finally {
    reading = false
  }
}

async function run(action: () => Promise<BrowserStatus>): Promise<void> {
  if (disposed || pending.value) return
  const current = ++generation
  pending.value = true
  actionError.value = null
  try {
    const reply = await action()
    if (!disposed && current === generation) {
      status.value = reply
      statusError.value = false
    }
  } catch (cause) {
    if (!disposed && current === generation) {
      actionError.value =
        typeof cause === 'string' && errorCodes.has(cause) ? cause : 'browser_unavailable'
    }
  } finally {
    if (!disposed && current === generation) pending.value = false
  }
}

function open(): void {
  const target = address.value.trim()
  if (!target || status.value?.sessionId) return
  void run(() => openBrowser(target))
}

function control(action: BrowserAction): void {
  const current = status.value
  if (!current?.sessionId || !canControl.value) return
  if (action === 'back' && (!current.historySupported || !current.canGoBack)) return
  if (action === 'forward' && (!current.historySupported || !current.canGoForward)) return
  void run(() => controlBrowser(current.sessionId!, action))
}

async function saveLink(): Promise<void> {
  const id = status.value?.sessionId
  const link = downloadAddress.value.trim()
  if (!id || !link || !status.value?.downloadsEnabled || downloading.value) return
  downloading.value = true
  downloadError.value = null
  downloadSaved.value = false
  try {
    const saved = await saveBrowserLink(id, link)
    if (!disposed && status.value?.sessionId === id) {
      downloadSaved.value = saved
      if (saved) downloadAddress.value = ''
    }
  } catch (cause) {
    if (!disposed && status.value?.sessionId === id) {
      downloadError.value =
        typeof cause === 'string' && downloadErrors.has(cause) ? cause : 'browser_download_failed'
    }
  } finally {
    if (!disposed) downloading.value = false
  }
}

watch(
  () => status.value?.sessionId,
  () => {
    downloadAddress.value = ''
    downloadError.value = null
    downloadSaved.value = false
  }
)

onMounted(() => {
  void refresh()
  timer = setInterval(() => {
    if (status.value?.sessionId) void refresh()
  }, 1000)
})
onUnmounted(() => {
  disposed = true
  generation++
  clearInterval(timer)
})
</script>

<template>
  <section id="isolated-browser" class="browser-panel" aria-labelledby="isolated-browser-heading">
    <h3 id="isolated-browser-heading">{{ t('tauriPreview.browser.title') }}</h3>
    <p class="section-description">{{ t('tauriPreview.browser.intro') }}</p>
    <form class="browser-form" @submit.prevent="open">
      <label for="isolated-browser-url">{{ t('tauriPreview.desktop.browserUrl') }}</label>
      <input
        id="isolated-browser-url"
        v-model="address"
        type="url"
        maxlength="2048"
        required
        :disabled="pending"
      />
      <AppButton
        data-browser-open
        html-type="submit"
        :disabled="pending || !!status?.sessionId || !address.trim()"
      >
        {{ t('tauriPreview.desktop.browserOpen') }}
      </AppButton>
    </form>
    <p role="status">{{ t(`tauriPreview.browser.${phase}`) }}</p>
    <dl v-if="status?.open" class="browser-location">
      <dt>{{ t('tauriPreview.browser.origin') }}</dt>
      <dd>{{ status.origin }}</dd>
      <dt>{{ t('tauriPreview.browser.address') }}</dt>
      <dd>{{ status.address }}</dd>
    </dl>
    <nav class="browser-actions" :aria-label="t('tauriPreview.browser.controls')">
      <AppButton
        data-browser-back
        :disabled="!canControl || !status?.historySupported || !status.canGoBack"
        @click="control('back')"
      >
        {{ t('tauriPreview.browser.back') }}
      </AppButton>
      <AppButton
        data-browser-forward
        :disabled="!canControl || !status?.historySupported || !status.canGoForward"
        @click="control('forward')"
      >
        {{ t('tauriPreview.browser.forward') }}
      </AppButton>
      <AppButton data-browser-reload :disabled="!canControl" @click="control('reload')">
        {{ t('tauriPreview.browser.reload') }}
      </AppButton>
      <AppButton data-browser-check :disabled="pending" @click="refresh">
        {{ t('tauriPreview.browser.check') }}
      </AppButton>
    </nav>
    <AppAlert v-if="statusError" type="error" :message="t('tauriPreview.browser.statusError')" />
    <AppAlert
      v-if="actionError"
      type="error"
      :message="t(`tauriPreview.browser.errors.${actionError}`)"
    />
    <AppAlert
      v-if="status?.navigationBlocked"
      type="warning"
      :message="t('tauriPreview.browser.navigationBlocked')"
    />
    <AppAlert
      v-if="status?.downloadBlocked"
      type="warning"
      :message="t('tauriPreview.browser.downloadBlocked')"
    />
    <p v-if="status?.open && !status.historySupported">
      {{ t('tauriPreview.browser.historyUnavailable') }}
    </p>
    <p class="section-description">{{ t('tauriPreview.browser.downloads') }}</p>
    <form
      v-if="status?.downloadsEnabled"
      class="browser-form"
      data-browser-save-link
      @submit.prevent="saveLink"
    >
      <label for="browser-download-url">{{ t('tauriPreview.browser.downloadUrl') }}</label>
      <input
        id="browser-download-url"
        v-model="downloadAddress"
        type="url"
        maxlength="2048"
        :disabled="downloading"
        required
      />
      <AppButton html-type="submit" :disabled="downloading || !downloadAddress.trim()">
        {{ t(downloading ? 'tauriPreview.browser.downloading' : 'tauriPreview.browser.saveLink') }}
      </AppButton>
      <p v-if="downloadSaved" role="status">{{ t('tauriPreview.browser.downloadSaved') }}</p>
      <AppAlert
        v-if="downloadError"
        type="error"
        :message="t(`tauriPreview.browser.errors.${downloadError}`)"
      />
    </form>
  </section>
</template>

<style scoped>
.browser-panel {
  margin-block-start: var(--space-5);
}
.browser-form {
  display: grid;
  justify-items: start;
  gap: var(--space-2);
  margin-block: var(--space-3);
}
.browser-form input {
  width: min(100%, calc(var(--space-12) * 9));
}
.browser-actions {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-2);
  margin-block: var(--space-3);
}
.browser-location {
  display: grid;
  gap: var(--space-1);
}
.browser-location dd {
  margin-inline-start: 0;
  overflow-wrap: anywhere;
  white-space: pre-wrap;
}
</style>
