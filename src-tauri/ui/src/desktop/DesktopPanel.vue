<script setup lang="ts">
import { invoke } from '@tauri-apps/api/core'
import { onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import AppAlert from '@renderer/components/AppAlert.vue'
import AppButton from '@renderer/components/AppButton.vue'
import ScreenRecorderPanel from './ScreenRecorderPanel.vue'

const { t, locale } = useI18n()
const tray = ref<boolean | null>(null)
const busy = ref(false)
const error = ref(false)

const address = ref('')
const opening = ref(false)
const browserError = ref<string | null>(null)
const browserErrors = new Set(['browser_invalid_url', 'browser_already_open'])

async function openBrowser(): Promise<void> {
  const target = address.value.trim()
  if (opening.value || !target) return
  opening.value = true
  browserError.value = null
  try {
    await invoke('open_isolated_browser', { address: target })
  } catch (cause) {
    browserError.value =
      typeof cause === 'string' && browserErrors.has(cause) ? cause : 'browser_unavailable'
  } finally {
    opening.value = false
  }
}

async function update(desired?: boolean): Promise<void> {
  if (busy.value) return
  busy.value = true
  error.value = false
  try {
    tray.value =
      desired === undefined
        ? await invoke<boolean>('desktop_tray')
        : await invoke<boolean>('set_desktop_tray', { enabled: desired, language: locale.value })
  } catch {
    error.value = true
  } finally {
    busy.value = false
  }
}

onMounted(() => update())
</script>

<template>
  <section id="desktop" class="panel" aria-labelledby="desktop-heading">
    <h2 id="desktop-heading">{{ t('tauriPreview.desktop.title') }}</h2>
    <p class="section-description">{{ t('tauriPreview.desktop.trayDetail') }}</p>
    <AppAlert v-if="error" type="error" :message="t('tauriPreview.desktop.trayError')" />
    <AppButton v-if="tray === null && error" :disabled="busy" @click="update()">
      {{ t('tauriPreview.desktop.retry') }}
    </AppButton>
    <AppButton v-if="tray !== null" :disabled="busy" :aria-pressed="tray" @click="update(!tray)">
      {{ t(tray ? 'tauriPreview.desktop.disableTray' : 'tauriPreview.desktop.enableTray') }}
    </AppButton>
    <form class="browser-form" @submit.prevent="openBrowser">
      <label for="isolated-browser-url">{{ t('tauriPreview.desktop.browserUrl') }}</label>
      <input
        id="isolated-browser-url"
        v-model="address"
        type="url"
        maxlength="2048"
        required
        :disabled="opening"
      />
      <AppButton html-type="submit" :disabled="opening || !address.trim()">
        {{ t('tauriPreview.desktop.browserOpen') }}
      </AppButton>
    </form>
    <AppAlert
      v-if="browserError"
      type="error"
      :message="t(`tauriPreview.desktop.browserErrors.${browserError}`)"
    />
    <ScreenRecorderPanel />
  </section>
</template>

<style scoped src="../panel.css"></style>

<style scoped>
.browser-form {
  display: grid;
  justify-items: start;
  gap: var(--space-2);
  margin-block: var(--space-5) var(--space-3);
}

.browser-form input {
  width: min(100%, calc(var(--space-12) * 9));
}
</style>
