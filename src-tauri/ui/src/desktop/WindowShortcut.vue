<script setup lang="ts">
import { invoke } from '@tauri-apps/api/core'
import { onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import AppAlert from '@renderer/components/AppAlert.vue'
import AppButton from '@renderer/components/AppButton.vue'

interface Shortcut {
  control: boolean
  option: boolean
  shift: boolean
  command: boolean
  key: string
}

const { t } = useI18n()
const active = ref<Shortcut | null>(null)
const chosen = ref<Shortcut>({
  control: false,
  option: true,
  shift: false,
  command: false,
  key: 'Backquote'
})
const busy = ref(false)
const error = ref<string | null>(null)

function label(shortcut: Shortcut): string {
  const modifiers = [
    shortcut.control && t('tauriPreview.desktop.shortcut.control'),
    shortcut.option && t('tauriPreview.desktop.shortcut.option'),
    shortcut.shift && t('tauriPreview.desktop.shortcut.shift'),
    shortcut.command && t('tauriPreview.desktop.shortcut.command')
  ].filter(Boolean)
  const key =
    shortcut.key === 'Backquote'
      ? '`'
      : shortcut.key === 'Space'
        ? t('tauriPreview.desktop.shortcut.space')
        : shortcut.key.replace(/^(Key|Digit)/, '')
  return [...modifiers, key].join(' + ')
}

function capture(event: KeyboardEvent): void {
  if (event.key === 'Escape' || event.key === 'Tab') return
  if (!/^(Backquote|Space|Key[A-Z]|Digit[0-9])$/.test(event.code)) return
  event.preventDefault()
  if (!(event.ctrlKey || event.altKey || event.metaKey)) {
    error.value = 'desktop_shortcut_invalid'
    return
  }
  error.value = null
  chosen.value = {
    control: event.ctrlKey,
    option: event.altKey,
    shift: event.shiftKey,
    command: event.metaKey,
    key: event.code
  }
}

async function update(action: () => Promise<Shortcut | null>): Promise<void> {
  if (busy.value) return
  busy.value = true
  error.value = null
  try {
    active.value = await action()
    if (active.value) chosen.value = active.value
  } catch (cause) {
    error.value =
      cause === 'desktop_shortcut_occupied' || cause === 'desktop_shortcut_invalid'
        ? cause
        : 'desktop_shortcut_unavailable'
  } finally {
    busy.value = false
  }
}

function setShortcut(shortcut: Shortcut | null): Promise<void> {
  return update(() => invoke('set_desktop_window_shortcut', { shortcut }))
}

onMounted(() => update(() => invoke('desktop_window_shortcut')))
</script>

<template>
  <fieldset class="shortcut-settings">
    <legend>{{ t('tauriPreview.desktop.shortcut.title') }}</legend>
    <label for="desktop-window-shortcut">{{ t('tauriPreview.desktop.shortcut.capture') }}</label>
    <input
      id="desktop-window-shortcut"
      readonly
      :value="label(chosen)"
      :disabled="busy"
      @keydown="capture"
    />
    <div class="shortcut-actions">
      <AppButton :disabled="busy" @click="setShortcut(chosen)">
        {{ t('tauriPreview.desktop.shortcut.apply') }}
      </AppButton>
      <AppButton v-if="active" :disabled="busy" @click="setShortcut(null)">
        {{ t('tauriPreview.desktop.shortcut.disable') }}
      </AppButton>
    </div>
    <p v-if="active" role="status">
      {{ t('tauriPreview.desktop.shortcut.active', { keys: label(active) }) }}
    </p>
    <AppAlert
      v-if="error"
      type="error"
      :message="t(`tauriPreview.desktop.shortcut.errors.${error}`)"
    />
  </fieldset>
</template>

<style scoped>
.shortcut-settings {
  display: grid;
  justify-items: start;
  gap: var(--space-2);
  border: 0;
  padding: 0;
  margin: var(--space-5) 0;
}

.shortcut-actions {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-2);
}
</style>
