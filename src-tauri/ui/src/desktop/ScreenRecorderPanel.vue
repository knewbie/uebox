<script setup lang="ts">
import { invoke } from '@tauri-apps/api/core'
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import AppAlert from '@renderer/components/AppAlert.vue'
import AppButton from '@renderer/components/AppButton.vue'
import AppCheckbox from '@renderer/components/AppCheckbox.vue'

type Phase =
  | 'idle'
  | 'selecting'
  | 'recording'
  | 'stopping'
  | 'completed'
  | 'cancelled'
  | 'failed'
  | 'unsupported'
  | 'busy'
interface RecordingStatus {
  supported: boolean
  token: string | null
  phase: Phase
  audio: boolean
  fileName: string | null
  error: string | null
  cleanupPending: boolean
}

const { t } = useI18n()
const status = ref<RecordingStatus | null>(null)
const audio = ref(false)
const pending = ref(false)
const error = ref(false)
const active = computed(() =>
  ['selecting', 'recording', 'stopping'].includes(status.value?.phase ?? '')
)
let reading = false
let generation = 0
let timer: ReturnType<typeof setInterval>

async function refresh(initial = false): Promise<void> {
  if (reading || pending.value) return
  const read = generation
  reading = true
  try {
    const reply = await invoke<RecordingStatus>('screen_recording_status')
    if (read === generation) status.value = reply
  } catch {
    if (initial && read === generation) error.value = true
  } finally {
    reading = false
  }
}

async function run(action: () => Promise<RecordingStatus | void>): Promise<void> {
  if (pending.value) return
  const current = ++generation
  pending.value = true
  error.value = false
  try {
    const reply = await action()
    if (reply && current === generation) status.value = reply
  } catch {
    if (current === generation) error.value = true
  } finally {
    pending.value = false
  }
}

function start(): Promise<void> {
  return run(() =>
    invoke<RecordingStatus>('start_screen_recording', { optIn: true, audio: audio.value })
  )
}

function stop(): Promise<void> {
  const token = status.value?.token
  if (!token) return Promise.resolve()
  return run(() => invoke<RecordingStatus>('stop_screen_recording', { token }))
}

function reveal(): Promise<void> {
  const token = status.value?.token
  if (!token) return Promise.resolve()
  return run(() => invoke<void>('reveal_screen_recording', { token }))
}

onMounted(() => {
  void refresh(true)
  timer = setInterval(() => {
    if (active.value) void refresh()
  }, 500)
})

onUnmounted(() => {
  generation++
  clearInterval(timer)
})
</script>

<template>
  <section class="screen-recorder" aria-labelledby="screen-recorder-title">
    <h3 id="screen-recorder-title">{{ t('tauriPreview.desktop.capture.title') }}</h3>
    <AppAlert v-if="error" type="error" :message="t('tauriPreview.desktop.capture.error')" />
    <AppButton v-if="!status && error" @click="refresh(true)">{{
      t('tauriPreview.desktop.retry')
    }}</AppButton>
    <p v-else-if="status && !status.supported">
      {{ t('tauriPreview.desktop.capture.unsupported') }}
    </p>
    <template v-else-if="status">
      <p class="section-description">{{ t('tauriPreview.desktop.capture.intro') }}</p>
      <div class="recording-actions">
        <AppCheckbox v-model:checked="audio" :disabled="pending || active">
          {{ t('tauriPreview.desktop.capture.audio') }}
        </AppCheckbox>
        <AppButton
          data-record-stop
          v-if="active"
          :disabled="pending || status.phase === 'stopping'"
          @click="stop"
        >
          {{
            t(
              status.phase === 'selecting'
                ? 'tauriPreview.desktop.capture.cancel'
                : 'tauriPreview.desktop.capture.stop'
            )
          }}
        </AppButton>
        <AppButton data-record-start v-else variant="primary" :disabled="pending" @click="start">
          {{ t('tauriPreview.desktop.capture.start') }}
        </AppButton>
        <AppButton v-if="status.fileName" :disabled="pending" @click="reveal">
          {{ t('tauriPreview.desktop.capture.reveal') }}
        </AppButton>
      </div>
      <p v-if="status.phase !== 'idle'" role="status">
        {{
          t(`tauriPreview.desktop.capture.phase.${status.phase}`, { name: status.fileName ?? '' })
        }}
      </p>
      <AppAlert
        v-if="status.cleanupPending"
        type="warning"
        :message="t('tauriPreview.desktop.capture.cleanup')"
      />
    </template>
  </section>
</template>

<style scoped>
.screen-recorder {
  margin-block-start: var(--space-5);
}

.recording-actions {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-3);
  align-items: center;
}
</style>
