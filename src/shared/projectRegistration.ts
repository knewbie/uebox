/** Bounded receipt for the explicit native project-registration export action. */
export type ProjectRegistrationExportResult =
  | { success: true; code: 'project_registration_exported'; count: number }
  | {
      success: false
      code:
        | 'project_registration_export_cancelled'
        | 'project_registration_export_busy'
        | 'project_registration_export_invalid'
        | 'project_registration_export_unavailable'
        | 'project_registration_export_exists'
        | 'project_registration_export_write_failed'
    }
