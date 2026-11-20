export type PortableModelExportFailure =
  | 'model_profile_export_cancelled'
  | 'model_profile_export_busy'
  | 'model_profile_export_invalid'
  | 'model_profile_export_unsupported_version'
  | 'model_profile_export_too_large'
  | 'model_profile_export_unavailable'
  | 'model_profile_export_exists'

export type PortableModelExportReply =
  | { success: true; profiles: number; unsupportedModels: number }
  | { success: false; code: PortableModelExportFailure }
