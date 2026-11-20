export type AssetIndexExportReply =
  | { success: true; folderCount: number; assetCount: number; indexAvailable: boolean }
  | { success: false; code: 'busy' | 'cancelled' | 'exists' | 'invalid' | 'unavailable' }
