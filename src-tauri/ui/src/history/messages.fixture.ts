/** Test-only translation fixtures; the parent task owns the application locales. */
export const historyTestMessages = {
  'en-US': {
    tauriPreview: {
      history: {
        title: 'Chat history',
        intro: 'Open saved chats for read-only viewing.',
        open: 'Open history',
        close: 'Close history',
        refresh: 'Refresh',
        loadMore: 'Load more',
        loadingList: 'Loading chats…',
        listError: 'Unable to load chat history. Try again.',
        emptyList: 'No saved chats.',
        chats: 'Saved chats',
        selectChat: 'Select a chat to view its turns.',
        loadingChat: 'Loading chat…',
        detailError: 'Unable to load this chat. Try again.',
        retry: 'Retry',
        untitled: 'Untitled chat',
        engine: 'Engine',
        createdAt: 'Created',
        updatedAt: 'Updated',
        finishedAt: 'Finished',
        unfinished: 'Not finished',
        turnTitle: 'Turn {number}',
        user: 'You',
        assistant: 'Assistant',
        noAssistantText: 'No assistant response saved.',
        emptyTurns: 'No turns saved in this chat.',
        actorReceipts: {
          title: 'Actor label receipts',
          approvalId: 'Approval ID',
          actualLabel: 'Read-back label',
          noActualLabel: 'No label read back',
          statuses: {
            applied: 'Applied',
            mismatch: 'Readback mismatch',
            not_executed: 'Not executed',
            unknown: 'Outcome unknown'
          }
        },
        engines: { unreal: 'Unreal Engine', unity: 'Unity', godot: 'Godot' },
        statuses: {
          pending: 'Pending',
          completed: 'Completed',
          failed: 'Failed',
          cancelled: 'Cancelled',
          interrupted: 'Interrupted'
        }
      }
    }
  },
  'zh-CN': {
    tauriPreview: {
      history: {
        title: '对话历史',
        intro: '打开已保存的对话，仅供只读查看。',
        open: '打开历史',
        close: '关闭历史',
        refresh: '刷新',
        loadMore: '加载更多',
        loadingList: '正在加载对话…',
        listError: '无法加载对话历史，请重试。',
        emptyList: '暂无已保存的对话。',
        chats: '已保存的对话',
        selectChat: '选择一段对话以查看内容。',
        loadingChat: '正在加载对话内容…',
        detailError: '无法加载此对话，请重试。',
        retry: '重试',
        untitled: '未命名对话',
        engine: '引擎',
        createdAt: '创建时间',
        updatedAt: '更新时间',
        finishedAt: '结束时间',
        unfinished: '尚未结束',
        turnTitle: '第 {number} 轮',
        user: '你',
        assistant: '助手',
        noAssistantText: '未保存助手回复。',
        emptyTurns: '此对话尚无已保存的轮次。',
        actorReceipts: {
          title: 'Actor 标签回执',
          approvalId: '审批 ID',
          actualLabel: '回读标签',
          noActualLabel: '未回读到标签',
          statuses: {
            applied: '已应用',
            mismatch: '回读不匹配',
            not_executed: '未执行',
            unknown: '结果未知'
          }
        },
        engines: { unreal: '虚幻引擎', unity: 'Unity', godot: 'Godot' },
        statuses: {
          pending: '处理中',
          completed: '已完成',
          failed: '失败',
          cancelled: '已取消',
          interrupted: '已中断'
        }
      }
    }
  }
}
