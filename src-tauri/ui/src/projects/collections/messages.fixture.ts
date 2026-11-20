/** Test fixtures and bilingual reference for the parent task's locale registration. */
export const projectCollectionsTestMessages = {
  'en-US': {
    tauriPreview: {
      projectCollections: {
        title: 'Project collections',
        intro: 'Group registered projects and arrange collections in the order you prefer.',
        refresh: 'Refresh',
        loading: 'Loading collections…',
        saving: 'Saving changes…',
        loadError: 'Unable to load collections. Refresh to try again.',
        changeError: 'Unable to update the collection. Try again.',
        empty: 'No collections yet. Create one to group your projects.',
        name: 'Collection name (optional)',
        create: 'Create collection',
        unnamed: 'Unnamed collection',
        list: 'Collections in display order',
        memberCount: '{count} projects',
        moveUp: 'Move up',
        moveDown: 'Move down',
        rename: 'Save name',
        members: 'Projects in this collection',
        membersHint: 'Check the registered projects that belong to this collection.',
        noProjects: 'Register a project before adding members.',
        missingProject: 'Unavailable registration: {id}',
        dissolve: 'Dissolve collection',
        confirmDissolve: 'Dissolve “{name}”? Project registrations and project files will be kept.',
        approveDissolve: 'Confirm dissolution',
        cancel: 'Cancel',
        importFile: 'Project collection export file',
        importAction: 'Import collections',
        imported: '{inserted} imported, {existing} already present.',
        importError: 'Unable to import the collection file. There is a 1 MiB limit.',
        importMissingProject:
          'Import the matching project registrations before importing their collections.',
        engines: { unreal: 'Unreal Engine', unity: 'Unity', godot: 'Godot' }
      }
    }
  },
  'zh-CN': {
    tauriPreview: {
      projectCollections: {
        title: '项目合集',
        intro: '将已登记的项目归入合集，并按需要调整合集顺序。',
        refresh: '刷新',
        loading: '正在加载合集…',
        saving: '正在保存更改…',
        loadError: '无法加载合集，请刷新重试。',
        changeError: '无法更新合集，请重试。',
        empty: '暂无合集，创建一个来整理项目。',
        name: '合集名称（可选）',
        create: '创建合集',
        unnamed: '未命名合集',
        list: '按显示顺序排列的合集',
        memberCount: '{count} 个项目',
        moveUp: '上移',
        moveDown: '下移',
        rename: '保存名称',
        members: '合集中的项目',
        membersHint: '勾选应属于此合集的已登记项目。',
        noProjects: '请先登记项目，再将其加入合集。',
        missingProject: '登记已不可用：{id}',
        dissolve: '解散合集',
        confirmDissolve: '确认解散「{name}」？项目登记和文件会保留。',
        approveDissolve: '确认解散',
        cancel: '取消',
        importFile: '已导出的项目合集文件',
        importAction: '导入合集',
        imported: '已导入 {inserted} 个，已有 {existing} 个。',
        importError: '无法导入项目合集文件。文件大小上限为 1 MiB。',
        importMissingProject: '请先导入对应的项目登记，再导入这些合集。',
        engines: { unreal: '虚幻引擎', unity: 'Unity', godot: 'Godot' }
      }
    }
  }
}
