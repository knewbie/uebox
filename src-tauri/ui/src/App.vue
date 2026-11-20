<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  PhBookOpen,
  PhArchive,
  PhChatCircleDots,
  PhClockCounterClockwise,
  PhFolderSimple,
  PhGear,
  PhTerminalWindow
} from '@phosphor-icons/vue'
import AppAlert from '@renderer/components/AppAlert.vue'
import AppButton from '@renderer/components/AppButton.vue'
import BrandMark from '@renderer/components/BrandMark.vue'
import { getHostStatus, type HostStatus } from './host'
import UnityPanel from './engines/unity/UnityPanel.vue'
import UnrealPanel from './engines/unreal/UnrealPanel.vue'
import GodotPanel from './engines/godot/GodotPanel.vue'
import ProjectLibraryPanel from './projects/ProjectLibraryPanel.vue'
import AgentHistoryPanel from './history/AgentHistoryPanel.vue'
import LegacyHistoryPanel from './legacy-history/LegacyHistoryPanel.vue'
import ModelSettingsPanel from './settings/ModelSettingsPanel.vue'
import AssetIndexPanel from './asset-index/AssetIndexPanel.vue'
import McpPanel from './mcp/McpPanel.vue'
import DesktopPanel from './desktop/DesktopPanel.vue'
import NotesPanel from './notes/NotesPanel.vue'
import NativeCliPanel from './native-cli/NativeCliPanel.vue'
import type { Locale } from './i18n'

type ThemePreference = 'system' | 'light' | 'dark'

const { t, locale } = useI18n()
const status = ref<HostStatus | null>(null)
const hostError = ref<string | null>(null)
const projectRegistrationAvailable = computed(() =>
  status.value?.capabilities.some(
    ({ id, available }) => id === 'projectRegistration' && available === true
  )
)
const chatHistoryAvailable = computed(() =>
  status.value?.capabilities.some(({ id, available }) => id === 'chatHistory' && available)
)
const legacyHistoryAvailable = computed(() =>
  status.value?.capabilities.some(({ id, available }) => id === 'legacyHistory' && available)
)
const projectCollectionsAvailable = computed(() =>
  status.value?.capabilities.some(({ id, available }) => id === 'projectCollections' && available)
)
const settingsAvailable = computed(() =>
  status.value?.capabilities.some(({ id, available }) => id === 'modelSettings' && available)
)
const assetIndexAvailable = computed(() =>
  status.value?.capabilities.some(({ id, available }) => id === 'assetIndex' && available)
)
const mcpAvailable = computed(() =>
  status.value?.capabilities.some(({ id, available }) => id === 'mcpInspection' && available)
)
const desktopAvailable = computed(() =>
  status.value?.capabilities.some(({ id, available }) => id === 'desktopPreferences' && available)
)
const noteLibraryAvailable = computed(() =>
  status.value?.capabilities.some(({ id, available }) => id === 'noteLibrary' && available)
)
const nativeCliAvailable = computed(() =>
  status.value?.capabilities.some(({ id, available }) => id === 'nativeCli' && available)
)
function readTheme(): ThemePreference {
  try {
    const value = localStorage.getItem('app-theme')
    return value === 'light' || value === 'dark' ? value : 'system'
  } catch {
    return 'system'
  }
}

const theme = ref<ThemePreference>(readTheme())
const colorScheme = window.matchMedia('(prefers-color-scheme: dark)')
const systemDark = ref(colorScheme.matches)
const onColorSchemeChange = (event: MediaQueryListEvent): void => {
  systemDark.value = event.matches
}

watch(
  [theme, systemDark],
  () => {
    document.documentElement.dataset.theme =
      theme.value === 'system' ? (systemDark.value ? 'dark' : 'light') : theme.value
  },
  { immediate: true }
)
watch(
  locale,
  (value) => {
    document.documentElement.lang = value
  },
  { immediate: true }
)

function chooseTheme(value: ThemePreference): void {
  theme.value = value
  try {
    localStorage.setItem('app-theme', value)
  } catch {
    /* preference is optional */
  }
}

function chooseLocale(value: Locale): void {
  locale.value = value
  try {
    localStorage.setItem('locale', value)
  } catch {
    /* preference is optional */
  }
}

onMounted(async () => {
  colorScheme.addEventListener('change', onColorSchemeChange)
  try {
    status.value = await getHostStatus()
  } catch (error) {
    hostError.value = String(error)
  }
})

onUnmounted(() => colorScheme.removeEventListener('change', onColorSchemeChange))
</script>

<template>
  <div class="shell">
    <aside class="sidebar">
      <div class="brand">
        <BrandMark class="brand__mark" />
        <div>
          <strong>{{ t('menu.appName') }}</strong>
          <span class="brand__subtitle">{{ t('tauriPreview.previewLabel') }}</span>
        </div>
      </div>
      <nav :aria-label="t('tauriPreview.capabilitiesTitle')">
        <ul class="navigation">
          <li v-if="projectRegistrationAvailable" class="navigation-link">
            <a href="#project-registration">
              <PhFolderSimple aria-hidden="true" />{{ t('menu.projectLib') }}
            </a>
          </li>
          <li v-else><PhFolderSimple aria-hidden="true" />{{ t('menu.projectLib') }}</li>
          <li><PhChatCircleDots aria-hidden="true" />{{ t('menu.aiAssistant') }}</li>
          <li v-if="chatHistoryAvailable" class="navigation-link">
            <a href="#agent-history">
              <PhClockCounterClockwise aria-hidden="true" />{{ t('tauriPreview.history.title') }}
            </a>
          </li>
          <li v-if="legacyHistoryAvailable" class="navigation-link">
            <a href="#legacy-history">
              <PhArchive aria-hidden="true" />{{ t('tauriPreview.legacyHistory.title') }}
            </a>
          </li>
          <li v-if="noteLibraryAvailable" class="navigation-link">
            <a href="#notes"><PhBookOpen aria-hidden="true" />{{ t('menu.notebooks') }}</a>
          </li>
          <li v-else><PhBookOpen aria-hidden="true" />{{ t('menu.notebooks') }}</li>
          <li v-if="settingsAvailable" class="navigation-link">
            <a href="#model-settings">
              <PhGear aria-hidden="true" />{{ t('tauriPreview.modelSettings.title') }}
            </a>
          </li>
          <li v-else><PhGear aria-hidden="true" />{{ t('tauriPreview.capabilities.settings') }}</li>
          <li v-if="assetIndexAvailable" class="navigation-link">
            <a href="#asset-index">
              <PhArchive aria-hidden="true" />{{ t('tauriPreview.assetIndex.title') }}
            </a>
          </li>
          <li v-if="nativeCliAvailable" class="navigation-link">
            <a href="#native-cli">
              <PhTerminalWindow aria-hidden="true" />{{ t('tauriPreview.nativeCli.title') }}
            </a>
          </li>
        </ul>
      </nav>
      <p class="sidebar__notice">{{ t('tauriPreview.unavailable') }}</p>
    </aside>

    <main class="content">
      <header class="toolbar">
        <div class="controls" role="group" :aria-label="t('tauriPreview.language')">
          <AppButton
            lang="zh-CN"
            size="small"
            :aria-pressed="locale === 'zh-CN'"
            :variant="locale === 'zh-CN' ? 'soft' : 'text'"
            @click="chooseLocale('zh-CN')"
            >{{ t('profile.appearance.languageZhCN') }}</AppButton
          >
          <AppButton
            lang="en-US"
            size="small"
            :aria-pressed="locale === 'en-US'"
            :variant="locale === 'en-US' ? 'soft' : 'text'"
            @click="chooseLocale('en-US')"
            >{{ t('profile.appearance.languageEnUS') }}</AppButton
          >
        </div>
        <div class="controls" role="group" :aria-label="t('tauriPreview.appearance')">
          <AppButton
            v-for="choice in ['system', 'light', 'dark'] as const"
            :key="choice"
            size="small"
            :data-theme-choice="choice"
            :aria-pressed="theme === choice"
            :variant="theme === choice ? 'soft' : 'text'"
            @click="chooseTheme(choice)"
            >{{ t(`tauriPreview.${choice}`) }}</AppButton
          >
        </div>
      </header>

      <div class="main-column">
        <p class="eyebrow">{{ t('tauriPreview.previewLabel') }}</p>
        <h1>{{ t('tauriPreview.title') }}</h1>
        <p class="lead">{{ t('tauriPreview.description') }}</p>
        <AppAlert
          type="warning"
          show-icon
          :message="t('tauriPreview.warning')"
          :description="
            t(
              status?.engineModules?.unity && status?.engineModules?.unreal
                ? 'tauriPreview.warningDetail'
                : 'tauriPreview.description'
            )
          "
        />

        <section class="panel" aria-labelledby="host-heading">
          <h2 id="host-heading">{{ t('tauriPreview.hostTitle') }}</h2>
          <p class="section-description">{{ t('tauriPreview.hostDetail') }}</p>
          <AppAlert
            v-if="hostError"
            type="error"
            show-icon
            :message="t('tauriPreview.hostError')"
            :description="t('tauriPreview.hostErrorDetail', { error: hostError })"
          />
          <p v-else-if="!status" role="status">{{ t('tauriPreview.loading') }}</p>
          <dl v-else class="host-details">
            <div>
              <dt>{{ t('tauriPreview.runtime') }}</dt>
              <dd>{{ status.backend }}</dd>
            </div>
            <div>
              <dt>{{ t('tauriPreview.process') }}</dt>
              <dd>{{ status.processId }}</dd>
            </div>
            <div>
              <dt>{{ t('tauriPreview.platform') }}</dt>
              <dd>{{ status.os }} · {{ status.architecture }}</dd>
            </div>
            <div>
              <dt>{{ t('tauriPreview.version') }}</dt>
              <dd>{{ status.version }}</dd>
            </div>
          </dl>
        </section>

        <ProjectLibraryPanel
          v-if="projectRegistrationAvailable"
          :collections-enabled="projectCollectionsAvailable === true"
        />
        <AgentHistoryPanel v-if="chatHistoryAvailable" />
        <LegacyHistoryPanel v-if="legacyHistoryAvailable" />
        <ModelSettingsPanel v-if="settingsAvailable" />
        <NotesPanel v-if="noteLibraryAvailable" />
        <NativeCliPanel v-if="nativeCliAvailable" />
        <DesktopPanel v-if="desktopAvailable" />
        <McpPanel v-if="mcpAvailable" />
        <AssetIndexPanel v-if="assetIndexAvailable" />

        <UnrealPanel
          v-if="!status || status.engineModules?.unreal === true"
          :host-ready="status?.engineModules?.unreal === true"
          :os="status?.os ?? null"
        />
        <section v-else class="panel" data-engine-unavailable="unreal">
          <h2>{{ t('tauriPreview.unreal.title') }}</h2>
          <p role="status" class="section-description">{{ t('tauriPreview.unavailable') }}</p>
        </section>
        <UnityPanel
          v-if="!status || status.engineModules?.unity === true"
          :os="status?.os ?? null"
        />
        <section v-else class="panel" data-engine-unavailable="unity">
          <h2>{{ t('tauriPreview.unity.title') }}</h2>
          <p role="status" class="section-description">{{ t('tauriPreview.unavailable') }}</p>
        </section>
        <GodotPanel
          v-if="!status || status.engineModules?.godot === true"
          :os="status?.os ?? null"
        />
        <section v-else class="panel" data-engine-unavailable="godot">
          <h2>{{ t('tauriPreview.godot.title') }}</h2>
          <p role="status" class="section-description">{{ t('tauriPreview.unavailable') }}</p>
        </section>

        <section v-if="status" class="panel" aria-labelledby="capabilities-heading">
          <h2 id="capabilities-heading">{{ t('tauriPreview.capabilitiesTitle') }}</h2>
          <ul class="capabilities">
            <li
              v-for="capability in status.capabilities"
              :key="capability.id"
              :data-capability-available="capability.available"
            >
              <span>{{ t(`tauriPreview.capabilities.${capability.id}`) }}</span>
              <span class="capabilities__status">{{
                t(capability.available ? 'tauriPreview.available' : 'tauriPreview.unavailable')
              }}</span>
            </li>
          </ul>
        </section>
      </div>
    </main>
  </div>
</template>

<style>
* {
  box-sizing: border-box;
}
html,
body,
#app {
  min-height: 100%;
  margin: 0;
}
body {
  color: var(--color-text-primary);
  background: var(--color-bg-page);
  font-family: var(--font-family-base);
  font-size: var(--font-size-base);
}
</style>

<style scoped>
.shell {
  display: flex;
  min-height: 100vh;
}
.sidebar {
  display: flex;
  flex-direction: column;
  flex: 0 0 15rem;
  padding: var(--space-6) var(--space-4);
  border-right: 1px solid var(--color-border-subtle);
  background: var(--color-bg-surface);
}
.brand {
  display: flex;
  align-items: center;
  gap: var(--space-3);
  margin-bottom: var(--space-8);
}
.brand__mark {
  width: 2rem;
  height: 2rem;
}
.brand__subtitle {
  display: block;
  margin-top: var(--space-1);
  color: var(--color-text-secondary);
  font-size: var(--font-size-xs);
}
.navigation,
.capabilities {
  list-style: none;
  padding: 0;
  margin: 0;
}
.navigation li {
  display: flex;
  align-items: center;
  gap: var(--space-3);
  padding: var(--space-3);
  color: var(--color-text-muted);
}
.navigation .navigation-link {
  padding: 0;
}
.navigation-link a {
  display: flex;
  align-items: center;
  gap: var(--space-3);
  width: 100%;
  padding: var(--space-3);
  color: var(--color-text-primary);
}
.sidebar__notice {
  margin: auto var(--space-3) 0;
  color: var(--color-text-secondary);
  font-size: var(--font-size-sm);
}
.content {
  min-width: 0;
  flex: 1;
  padding: var(--space-6) var(--space-8) var(--space-12);
}
.toolbar {
  display: flex;
  justify-content: flex-end;
  flex-wrap: wrap;
  gap: var(--space-4);
}
.controls {
  display: flex;
  gap: var(--space-1);
}
.main-column {
  max-width: 52rem;
  margin: var(--space-10) auto 0;
}
.eyebrow {
  margin: 0 0 var(--space-2);
  color: var(--color-text-secondary);
  font-size: var(--font-size-sm);
}
h1 {
  margin: 0 0 var(--space-3);
  font-size: var(--font-size-3xl);
  line-height: var(--line-height-tight);
}
.lead {
  color: var(--color-text-secondary);
  line-height: var(--line-height-normal);
  margin: 0 0 var(--space-6);
}
.host-details {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(10rem, 1fr));
  gap: var(--space-5);
  margin: 0;
}
.host-details dt {
  color: var(--color-text-secondary);
  font-size: var(--font-size-sm);
}
.host-details dd {
  margin: var(--space-2) 0 0;
  font-family: var(--font-family-mono);
  overflow-wrap: anywhere;
}
.capabilities {
  margin-top: var(--space-4);
}
.capabilities li {
  display: flex;
  justify-content: space-between;
  gap: var(--space-4);
  padding: var(--space-3) 0;
  border-top: 1px solid var(--color-border-subtle);
}
.capabilities__status {
  color: var(--color-text-secondary);
}
@media (max-width: 760px) {
  .sidebar {
    flex-basis: 11rem;
  }
  .content {
    padding: var(--space-4);
  }
}
</style>

<style scoped src="./panel.css"></style>
