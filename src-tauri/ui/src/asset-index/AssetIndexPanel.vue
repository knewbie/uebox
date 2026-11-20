<script setup lang="ts">
import { invoke } from '@tauri-apps/api/core'
import { computed, onMounted, ref, shallowRef } from 'vue'
import { useI18n } from 'vue-i18n'
import AppAlert from '@renderer/components/AppAlert.vue'
import AppButton from '@renderer/components/AppButton.vue'
import { ManifestReadError, readLegacyManifest } from '../legacy-history/manifest'

type Entry = { identity: string; name: string }
type Vault = Entry & { rootPath: string; indexAvailable: boolean }
type Asset = Entry & { relativePath: string | null; size: number | null; tags: string[] }
type Page<T> = { items: T[]; nextId: string | null }
type ImportReport = { assets: number; folders: number; retainedIndex: boolean }

const limit = 25
const { t } = useI18n()
const vaults = ref<Vault[]>([])
const vaultsNext = ref<string | null>(null)
const selection = ref<string | null>(null)
const selected = computed(() => vaults.value.find(({ identity }) => identity === selection.value))
const path = ref<Entry[]>([])
const folders = ref<Entry[]>([])
const assets = ref<Asset[]>([])
const foldersNext = ref<string | null>(null)
const assetsNext = ref<string | null>(null)
const searchText = ref('')
const search = ref<string | null>(null)
const loading = ref(false)
const loadingVaults = ref(false)
const listError = ref(false)
const picker = ref<HTMLInputElement | null>(null)
const archive = shallowRef<File | null>(null)
const report = ref<ImportReport | null>(null)
const importing = ref(false)
const transferError = ref<string | null>(null)
let directoryRequest = 0

const openingOriginal = ref<string | null>(null)
const originalError = ref<string | null>(null)

async function revealOriginal(asset: Asset): Promise<void> {
  const library = selected.value
  if (!library || !asset.relativePath || openingOriginal.value !== null) return
  openingOriginal.value = asset.identity
  originalError.value = null
  try {
    await invoke('reveal_asset_original', { vaultId: library.identity, assetId: asset.identity })
  } catch (cause) {
    if (selection.value === library.identity) {
      originalError.value =
        cause === 'asset_original_offline' || cause === 'asset_original_outside'
          ? cause
          : 'asset_original_unavailable'
    }
  } finally {
    openingOriginal.value = null
  }
}

const transferErrors = new Set([
  'asset_index_conflict',
  'asset_index_limit',
  'asset_index_version',
  'asset_index_recovery_required'
])

function selectFile(event: Event): void {
  archive.value = (event.target as HTMLInputElement).files?.[0] ?? null
  report.value = null
  transferError.value = null
}

async function readVaults(more = false): Promise<void> {
  if (loadingVaults.value) return
  loadingVaults.value = true
  listError.value = false
  try {
    const result = await invoke<Page<Vault>>('list_asset_vaults', {
      afterId: more ? vaultsNext.value : null,
      limit
    })
    vaultsNext.value = result.nextId
    vaults.value = more ? [...vaults.value, ...result.items] : result.items
    if (selection.value && !selected.value) {
      selection.value = null
      await resetDirectory()
    }
  } catch {
    listError.value = true
  } finally {
    loadingVaults.value = false
  }
}

async function importArchive(): Promise<void> {
  const file = archive.value
  if (!file || importing.value || loadingVaults.value) return
  importing.value = true
  transferError.value = null
  try {
    const document = await readLegacyManifest(file)
    report.value = await invoke<ImportReport>('import_asset_index', { document })
    archive.value = null
    if (picker.value) picker.value.value = ''
    await readVaults()
    await openVault(selection.value)
  } catch (cause) {
    transferError.value =
      cause instanceof ManifestReadError && cause.code === 'tooLarge'
        ? 'asset_index_limit'
        : typeof cause === 'string' && transferErrors.has(cause)
          ? cause
          : 'generic'
  } finally {
    importing.value = false
  }
}

async function readDirectory(mode: 'reset' | 'folders' | 'assets'): Promise<void> {
  if (!selected.value?.indexAvailable || (mode !== 'reset' && loading.value)) return
  const request = ++directoryRequest
  const vaultId = selected.value.identity
  const folderId = path.value.at(-1)?.identity ?? null
  const term = search.value
  loading.value = true
  listError.value = false
  try {
    const [folderPage, assetPage] = await Promise.all([
      term === null && mode !== 'assets'
        ? invoke<Page<Entry>>('list_asset_folders', {
            vaultId,
            folderId,
            afterId: mode === 'folders' ? foldersNext.value : null,
            limit
          })
        : null,
      mode !== 'folders'
        ? invoke<Page<Asset>>('list_asset_items', {
            vaultId,
            folderId,
            afterId: mode === 'assets' ? assetsNext.value : null,
            limit,
            search: term
          })
        : null
    ])
    if (request !== directoryRequest) return
    if (folderPage) {
      folders.value =
        mode === 'folders' ? [...folders.value, ...folderPage.items] : folderPage.items
      foldersNext.value = folderPage.nextId
    }
    if (assetPage) {
      assets.value = mode === 'assets' ? [...assets.value, ...assetPage.items] : assetPage.items
      assetsNext.value = assetPage.nextId
    }
  } catch {
    if (request === directoryRequest) listError.value = true
  } finally {
    if (request === directoryRequest) loading.value = false
  }
}

function resetDirectory(): Promise<void> {
  directoryRequest++
  loading.value = false
  folders.value = []
  assets.value = []
  foldersNext.value = null
  assetsNext.value = null
  return readDirectory('reset')
}

function openVault(id: string | null): Promise<void> {
  selection.value = id
  originalError.value = null
  path.value = []
  searchText.value = ''
  search.value = null
  return resetDirectory()
}

function navigate(foldersPath: Entry[]): Promise<void> {
  path.value = foldersPath
  searchText.value = ''
  search.value = null
  return resetDirectory()
}

function searchVault(): Promise<void> {
  search.value = searchText.value.trim() || null
  return resetDirectory()
}

onMounted(() => readVaults())
</script>

<template>
  <section id="asset-index" class="panel" aria-labelledby="asset-index-heading">
    <h2 id="asset-index-heading">{{ t('tauriPreview.assetIndex.title') }}</h2>
    <p class="section-description">{{ t('tauriPreview.assetIndex.intro') }}</p>
    <div class="transfer">
      <label for="asset-index-archive">{{ t('tauriPreview.assetIndex.selectArchive') }}</label>
      <input
        id="asset-index-archive"
        ref="picker"
        type="file"
        accept=".json,application/json"
        :disabled="importing"
        @change="selectFile"
      />
      <template v-if="archive">
        <p>{{ t('tauriPreview.assetIndex.confirm', { name: archive.name }) }}</p>
        <AppButton :disabled="importing || loadingVaults" @click="importArchive">
          {{
            t(importing ? 'tauriPreview.assetIndex.importing' : 'tauriPreview.assetIndex.import')
          }}
        </AppButton>
      </template>
      <AppAlert
        v-if="transferError"
        type="error"
        :message="t(`tauriPreview.assetIndex.errors.${transferError}`)"
      />
      <p v-if="report" role="status">
        {{
          t(
            report.retainedIndex
              ? 'tauriPreview.assetIndex.retained'
              : 'tauriPreview.assetIndex.imported',
            report
          )
        }}
      </p>
    </div>

    <AppAlert v-if="listError" type="error" :message="t('tauriPreview.assetIndex.loadError')" />
    <AppButton v-if="listError && !selected" :disabled="loadingVaults" @click="readVaults()">
      {{ t('tauriPreview.assetIndex.retry') }}
    </AppButton>
    <p v-if="!loadingVaults && !vaults.length">{{ t('tauriPreview.assetIndex.noVaults') }}</p>
    <div v-if="vaults.length" class="vault-picker">
      <label for="asset-index-vault">{{ t('tauriPreview.assetIndex.vault') }}</label>
      <select
        id="asset-index-vault"
        :value="selection ?? ''"
        :disabled="importing"
        @change="openVault(($event.target as HTMLSelectElement).value || null)"
      >
        <option value="">{{ t('tauriPreview.assetIndex.selectVault') }}</option>
        <option v-for="vault in vaults" :key="vault.identity" :value="vault.identity">
          {{ vault.name }} · {{ vault.rootPath }}
        </option>
      </select>
      <AppButton v-if="vaultsNext" :disabled="loadingVaults || importing" @click="readVaults(true)">
        {{ t('tauriPreview.assetIndex.moreVaults') }}
      </AppButton>
    </div>

    <template v-if="selected">
      <p>
        <code>{{ selected.rootPath }}</code>
      </p>
      <p v-if="!selected.indexAvailable">{{ t('tauriPreview.assetIndex.registrationOnly') }}</p>
      <template v-else>
        <form class="asset-search" @submit.prevent="searchVault">
          <label for="asset-index-search">{{ t('tauriPreview.assetIndex.searchLabel') }}</label>
          <input id="asset-index-search" v-model="searchText" type="search" maxlength="64" />
          <AppButton html-type="submit" :disabled="importing">{{
            t('tauriPreview.assetIndex.search')
          }}</AppButton>
        </form>
        <div v-if="search === null" class="folder-path">
          <AppButton size="small" @click="navigate([])">{{
            t('tauriPreview.assetIndex.root')
          }}</AppButton>
          <AppButton
            v-for="(folder, index) in path"
            :key="folder.identity"
            size="small"
            @click="navigate(path.slice(0, index + 1))"
            >{{ folder.name }}</AppButton
          >
        </div>
        <p v-if="loading" role="status">{{ t('tauriPreview.assetIndex.loading') }}</p>
        <AppButton v-if="listError" :disabled="loading" @click="resetDirectory">{{
          t('tauriPreview.assetIndex.retry')
        }}</AppButton>
        <ul v-if="folders.length">
          <li v-for="folder in folders" :key="folder.identity">
            <AppButton variant="text" @click="navigate([...path, folder])">{{
              folder.name
            }}</AppButton>
          </li>
        </ul>
        <AppButton v-if="foldersNext" :disabled="loading" @click="readDirectory('folders')">{{
          t('tauriPreview.assetIndex.moreFolders')
        }}</AppButton>
        <p v-if="!loading && !listError && !assets.length && !folders.length">
          {{ t('tauriPreview.assetIndex.empty') }}
        </p>
        <AppAlert
          v-if="originalError"
          type="error"
          :message="t(`tauriPreview.assetIndex.errors.${originalError}`)"
        />
        <ul v-if="assets.length" class="assets-list">
          <li v-for="asset in assets" :key="asset.identity">
            <strong>{{ asset.name }}</strong>
            <code v-if="asset.relativePath">{{ asset.relativePath }}</code>
            <span v-if="asset.tags.length">{{ asset.tags.join(' · ') }}</span>
            <AppButton
              v-if="asset.relativePath"
              data-reveal-asset
              variant="text"
              size="small"
              :disabled="openingOriginal !== null"
              @click="revealOriginal(asset)"
              >{{ t('tauriPreview.assetIndex.revealOriginal') }}</AppButton
            >
          </li>
        </ul>
        <AppButton v-if="assetsNext" :disabled="loading" @click="readDirectory('assets')">{{
          t('tauriPreview.assetIndex.moreAssets')
        }}</AppButton>
      </template>
    </template>
  </section>
</template>

<style scoped src="../panel.css"></style>
<style scoped>
.transfer,
.vault-picker,
.asset-search {
  display: grid;
  justify-items: start;
  gap: var(--space-2);
  margin-block: var(--space-4);
}

.folder-path {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-2);
}

.assets-list li {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-2);
  padding-block: var(--space-2);
}

code {
  overflow-wrap: anywhere;
}
</style>
