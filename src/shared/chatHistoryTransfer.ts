export type ChatHistoryExportCode =
  | 'chat_history_export_cancelled'
  | 'chat_history_export_busy'
  | 'chat_history_export_invalid'
  | 'chat_history_export_unavailable'
  | 'chat_history_export_exists'
  | 'chat_history_export_write_failed'

export type ChatHistoryExportResult =
  | { success: true; code: 'chat_history_exported'; count: number }
  | { success: false; code: ChatHistoryExportCode }
