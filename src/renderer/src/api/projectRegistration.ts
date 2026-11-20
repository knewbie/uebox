/**
 * User-initiated export returns a bounded receipt instead of unwrapResult():
 * the UI must distinguish cancellation, an existing destination and other failures.
 * No native path or raw IPC error is returned to the caller.
 */
import { z } from 'zod'
import type { ProjectRegistrationExportResult } from '../../../shared/projectRegistration'

export type { ProjectRegistrationExportResult }

const receipt = z.discriminatedUnion('success', [
  z.object({
    success: z.literal(true),
    code: z.literal('project_registration_exported'),
    count: z.number().int().min(0).max(4096)
  }),
  z.object({
    success: z.literal(false),
    code: z.enum([
      'project_registration_export_cancelled',
      'project_registration_export_busy',
      'project_registration_export_invalid',
      'project_registration_export_unavailable',
      'project_registration_export_exists',
      'project_registration_export_write_failed'
    ])
  })
])

export const projectRegistrationAPI = {
  async exportRegistration(): Promise<ProjectRegistrationExportResult> {
    try {
      const result = await window.api?.database?.project?.exportRegistration?.()
      const parsed = receipt.safeParse(result)
      if (parsed.success) return parsed.data
    } catch {
      // Rejected IPC can include native paths; expose only the fixed unavailable code.
    }
    return { success: false, code: 'project_registration_export_unavailable' }
  }
}
