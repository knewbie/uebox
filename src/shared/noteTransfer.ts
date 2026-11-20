export type NoteTransferReply =
  | { success: true; count: number; skipped: number }
  | { success: false; code: 'busy' | 'cancelled' | 'exists' | 'invalid' | 'unavailable' }
