<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import AInput from 'ant-design-vue/es/input/Input'
import AppAlert from '@renderer/components/AppAlert.vue'
import AppButton from '@renderer/components/AppButton.vue'
import AppCheckbox from '@renderer/components/AppCheckbox.vue'
import { inspectUnity, type UnityInspection } from './commands'

const props = defineProps<{ os: string | null }>()
const { t } = useI18n()
const unityProject = ref('')
const unityPort = ref('')
const unityTakeoverAllowed = ref(false)
const unityLoading = ref(false)
const unityError = ref<string | null>(null)
const unityResult = ref<UnityInspection | null>(null)

const validUnityProject = computed(() => {
  const path = unityProject.value
  if (props.os === null || path.includes('\0')) return false
  if (props.os === 'windows') {
    const normalized = path.replace(/\\/g, '/')
    return (
      /^(?:[a-zA-Z]:\/|\/\/[^/]+\/[^/]+)/.test(normalized) && !normalized.split('/').includes('..')
    )
  }
  return path.startsWith('/') && !path.split('/').includes('..')
})
const validUnityPort = computed(() => {
  if (!/^\d{1,5}$/.test(unityPort.value)) return false
  const port = Number(unityPort.value)
  return port >= 1 && port <= 65535
})
const canQueryUnity = computed(
  () =>
    validUnityProject.value &&
    validUnityPort.value &&
    unityTakeoverAllowed.value &&
    !unityLoading.value
)

watch(
  [unityProject, unityPort],
  () => {
    unityTakeoverAllowed.value = false
    unityResult.value = null
    unityError.value = null
  },
  { flush: 'sync' }
)

async function queryUnity(): Promise<void> {
  if (!canQueryUnity.value) return
  unityLoading.value = true
  unityResult.value = null
  unityError.value = null
  try {
    unityResult.value = await inspectUnity(
      unityProject.value,
      Number(unityPort.value),
      unityTakeoverAllowed.value
    )
  } catch (error) {
    unityError.value = String(error)
  } finally {
    unityTakeoverAllowed.value = false
    unityLoading.value = false
  }
}
</script>

<template>
  <section class="panel" aria-labelledby="unity-heading">
    <h2 id="unity-heading">{{ t('tauriPreview.unity.title') }}</h2>
    <p class="section-description">{{ t('tauriPreview.unity.intro') }}</p>
    <AppAlert
      type="warning"
      show-icon
      :message="t('tauriPreview.unity.warning')"
      :description="t('tauriPreview.unity.warningDetail')"
    />
    <p class="section-description">{{ t('tauriPreview.unity.trust') }}</p>
    <p class="section-description">{{ t('tauriPreview.unity.preferences') }}</p>
    <form class="unity-form" @submit.prevent="queryUnity">
      <label for="unity-project">{{ t('tauriPreview.unity.projectPath') }}</label>
      <AInput
        id="unity-project"
        v-model:value="unityProject"
        name="unity-project"
        autocomplete="off"
        :disabled="unityLoading"
      />
      <label for="unity-port">{{ t('tauriPreview.unity.port') }}</label>
      <AInput
        id="unity-port"
        v-model:value="unityPort"
        name="unity-port"
        inputmode="numeric"
        autocomplete="off"
        aria-describedby="unity-port-hint"
        :disabled="unityLoading"
      />
      <p id="unity-port-hint" class="unity-hint">{{ t('tauriPreview.unity.portHint') }}</p>
      <AppCheckbox v-model:checked="unityTakeoverAllowed" :disabled="unityLoading">
        {{ t('tauriPreview.unity.confirm') }}
      </AppCheckbox>
      <AppButton
        data-unity-query
        html-type="submit"
        variant="primary"
        :disabled="!canQueryUnity"
        :loading="unityLoading"
        >{{ t('tauriPreview.unity.query') }}</AppButton
      >
    </form>
    <p v-if="unityLoading" role="status">{{ t('tauriPreview.unity.reading') }}</p>
    <AppAlert
      v-if="unityError"
      type="error"
      show-icon
      :message="t('tauriPreview.unity.error')"
      :description="unityError"
    />
    <div v-if="unityResult" class="unity-result" data-unity-result>
      <h3>{{ t('tauriPreview.unity.project') }}</h3>
      <dl class="unity-details">
        <div>
          <dt>{{ t('tauriPreview.unity.project') }}</dt>
          <dd>{{ unityResult.project.projectName }}</dd>
        </div>
        <div>
          <dt>{{ t('tauriPreview.unity.root') }}</dt>
          <dd>{{ unityResult.project.projectRoot }}</dd>
        </div>
        <div>
          <dt>{{ t('tauriPreview.unity.version') }}</dt>
          <dd>{{ unityResult.project.unityVersion }}</dd>
        </div>
        <div>
          <dt>{{ t('tauriPreview.unity.assets') }}</dt>
          <dd>{{ unityResult.project.assetsPath }}</dd>
        </div>
        <div>
          <dt>{{ t('tauriPreview.unity.platform') }}</dt>
          <dd>{{ unityResult.project.platform }}</dd>
        </div>
        <div>
          <dt>{{ t('tauriPreview.unity.pipeline') }}</dt>
          <dd>{{ unityResult.project.renderPipeline }}</dd>
        </div>
      </dl>
      <h3>{{ t('tauriPreview.unity.scene') }}</h3>
      <dl class="unity-details">
        <div>
          <dt>{{ t('tauriPreview.unity.scene') }}</dt>
          <dd>{{ unityResult.scene.name || t('tauriPreview.unity.empty') }}</dd>
        </div>
        <div>
          <dt>{{ t('tauriPreview.unity.scenePath') }}</dt>
          <dd>{{ unityResult.scene.path || t('tauriPreview.unity.empty') }}</dd>
        </div>
        <div>
          <dt>{{ t('tauriPreview.unity.buildIndex') }}</dt>
          <dd>{{ unityResult.scene.buildIndex }}</dd>
        </div>
        <div>
          <dt>{{ t('tauriPreview.unity.dirty') }}</dt>
          <dd>
            {{ t(unityResult.scene.isDirty ? 'tauriPreview.unity.yes' : 'tauriPreview.unity.no') }}
          </dd>
        </div>
        <div>
          <dt>{{ t('tauriPreview.unity.loaded') }}</dt>
          <dd>
            {{ t(unityResult.scene.isLoaded ? 'tauriPreview.unity.yes' : 'tauriPreview.unity.no') }}
          </dd>
        </div>
        <div>
          <dt>{{ t('tauriPreview.unity.roots') }}</dt>
          <dd>{{ unityResult.scene.rootCount }}</dd>
        </div>
      </dl>
    </div>
  </section>
</template>

<style scoped src="../../panel.css"></style>

<style scoped>
.unity-form {
  display: grid;
  gap: var(--space-3);
  margin-bottom: var(--space-5);
}
.unity-form :deep(.ant-input) {
  color: var(--color-text-primary);
  background: var(--color-bg-page);
  border-color: var(--color-border);
}
.unity-form :deep(.ant-input:focus-visible) {
  outline: 2px solid var(--color-border-focus);
  outline-offset: 2px;
}
.unity-form .app-button {
  justify-self: start;
}
.unity-hint {
  margin: 0;
  color: var(--color-text-secondary);
  font-size: var(--font-size-sm);
}
.unity-result h3 {
  margin: var(--space-6) 0 var(--space-4);
  font-size: var(--font-size-md);
}
.unity-details {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(12rem, 1fr));
  gap: var(--space-4);
  margin: 0;
}
.unity-details dt {
  color: var(--color-text-secondary);
  font-size: var(--font-size-sm);
}
.unity-details dd {
  margin: var(--space-1) 0 0;
  overflow-wrap: anywhere;
}
</style>
