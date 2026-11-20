/** Test-only bilingual reference; the parent task owns application locale registration. */
export const legacyHistoryTestMessages = {
  'en-US': {
    tauriPreview: {
      legacyHistory: {
        title: 'Legacy conversation archive',
        intro: 'Import an old JSON archive and browse its saved conversations as read-only text.',
        open: 'Open archive',
        close: 'Close archive',
        importFile: 'Legacy JSON archive',
        importHint: 'Choose a UTF-8 JSON file up to 16 MiB, then confirm import.',
        confirmImport: 'Confirm import',
        clearFile: 'Cancel selection',
        importing: 'Importing archive…',
        imported:
          '{inserted} imported; {alreadyImported} already imported; {embeddedAttachments} embedded attachments; {unavailableAttachments} unavailable attachments.',
        errors: {
          tooLarge: 'The archive exceeds the 16 MiB limit.',
          invalidUtf8: 'The archive must use valid UTF-8 encoding.',
          readFailed: 'Unable to read the selected archive. Select the file again.',
          importFailed: 'Unable to import the archive. Check the file and try again.'
        },
        refresh: 'Refresh',
        loadMore: 'Load more',
        loadingList: 'Loading archived conversations…',
        listError: 'Unable to load the archive. Try again.',
        emptyList: 'No imported conversations.',
        chats: 'Archived conversations',
        selectChat: 'Select an archived conversation to view its messages.',
        loadingChat: 'Loading archived conversation…',
        detailError: 'Unable to load this conversation. Try again.',
        retry: 'Retry',
        untitled: 'Untitled conversation',
        createdAt: 'Created',
        updatedAt: 'Updated',
        emptyMessages: 'No messages saved in this conversation.',
        roles: { user: 'You', assistant: 'Assistant' },
        statuses: {
          completed: 'Completed',
          failed: 'Failed',
          cancelled: 'Cancelled',
          interrupted: 'Interrupted'
        },
        attachments: {
          title: 'Attachments',
          kind: 'Attachment: {kind}',
          unavailable: 'Attachment unavailable',
          viewImage: 'View image',
          loadingImage: 'Loading image…',
          imageError: 'Unable to display this image. Try again.',
          retryImage: 'Retry image',
          imageAlt: 'Archived image'
        }
      }
    }
  },
  'zh-CN': {
    tauriPreview: {
      legacyHistory: {
        title: '旧版对话归档',
        intro: '导入旧版 JSON 归档，以只读纯文本查看已保存的对话。',
        open: '打开归档',
        close: '关闭归档',
        importFile: '旧版 JSON 归档',
        importHint: '选择不超过 16 MiB 的 UTF-8 JSON 文件，再确认导入。',
        confirmImport: '确认导入',
        clearFile: '取消选择',
        importing: '正在导入归档…',
        imported:
          '已导入 {inserted} 条；已存在 {alreadyImported} 条；内嵌附件 {embeddedAttachments} 个；不可用附件 {unavailableAttachments} 个。',
        errors: {
          tooLarge: '归档文件超过 16 MiB 上限。',
          invalidUtf8: '归档文件必须使用有效的 UTF-8 编码。',
          readFailed: '无法读取所选归档，请重新选择文件。',
          importFailed: '无法导入归档，请检查文件后重试。'
        },
        refresh: '刷新',
        loadMore: '加载更多',
        loadingList: '正在加载归档对话…',
        listError: '无法加载归档，请重试。',
        emptyList: '暂无已导入的对话。',
        chats: '归档对话',
        selectChat: '选择一段归档对话以查看消息。',
        loadingChat: '正在加载归档对话内容…',
        detailError: '无法加载此对话，请重试。',
        retry: '重试',
        untitled: '未命名对话',
        createdAt: '创建时间',
        updatedAt: '更新时间',
        emptyMessages: '此对话没有已保存的消息。',
        roles: { user: '你', assistant: '助手' },
        statuses: {
          completed: '已完成',
          failed: '失败',
          cancelled: '已取消',
          interrupted: '已中断'
        },
        attachments: {
          title: '附件',
          kind: '附件：{kind}',
          unavailable: '附件不可用',
          viewImage: '查看图片',
          loadingImage: '正在加载图片…',
          imageError: '无法显示此图片，请重试。',
          retryImage: '重试图片',
          imageAlt: '归档图片'
        }
      }
    }
  }
}
