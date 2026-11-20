export type ProjectCollectionsExportResult =
  | { success: true; count: number }
  | { success: false; code: 'cancelled' | 'busy' | 'invalid' | 'exists' | 'unavailable' }
