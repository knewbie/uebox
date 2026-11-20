<script setup lang="ts">
import { invoke } from '@tauri-apps/api/core'
import { onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import AppAlert from '@renderer/components/AppAlert.vue'
import AppButton from '@renderer/components/AppButton.vue'
import ScreenRecorderPanel from './ScreenRecorderPanel.vue'
import BrowserPanel from '../browser/BrowserPanel.vue'
import WindowShortcut from './WindowShortcut.vue'

const { t, locale } = useI18n()
const tray = ref<boolean | null>(null)
const editingShortcut = ref(false)
const busy = ref(false)
const error = ref(false)

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
    <AppButton
      :aria-expanded="editingShortcut"
      data-desktop-shortcut
      @click="editingShortcut = !editingShortcut"
    >
      {{ t('tauriPreview.desktop.shortcut.configure') }}
    </AppButton>
    <WindowShortcut v-if="editingShortcut" />
    <BrowserPanel />
    <ScreenRecorderPanel />
  </section>
</template>

<style scoped src="../panel.css"></style>
