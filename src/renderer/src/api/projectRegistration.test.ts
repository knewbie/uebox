import { afterEach, expect, it, vi } from 'vitest'
import { projectRegistrationAPI } from './projectRegistration'

const original = window.api.database.project

afterEach(() => {
  window.api.database.project = original
})

function bridge(result: unknown): ReturnType<typeof vi.fn> {
  const exportRegistration = vi.fn().mockResolvedValue(result)
  window.api.database.project = { ...original, exportRegistration }
  return exportRegistration
}

it('calls the dedicated no-argument bridge and returns only the safe success receipt', async () => {
  const call = bridge({
    success: true,
    code: 'project_registration_exported',
    count: 2,
    path: '/private/fixture'
  })
  expect(await projectRegistrationAPI.exportRegistration()).toEqual({
    success: true,
    code: 'project_registration_exported',
    count: 2
  })
  expect(call).toHaveBeenCalledExactlyOnceWith()
})

it.each([
  'project_registration_export_cancelled',
  'project_registration_export_busy',
  'project_registration_export_invalid',
  'project_registration_export_unavailable',
  'project_registration_export_exists',
  'project_registration_export_write_failed'
])('preserves the fixed failure code %s without throwing', async (code) => {
  bridge({ success: false, code, error: 'fixture-secret' })
  expect(await projectRegistrationAPI.exportRegistration()).toEqual({ success: false, code })
})

it.each([
  undefined,
  { success: false, code: '/private/fixture-secret' },
  { success: true, code: 'project_registration_exported', count: -1 },
  { success: true, code: 'project_registration_exported', count: 4097 }
])('maps malformed responses to unavailable (%#)', async (result) => {
  bridge(result)
  expect(await projectRegistrationAPI.exportRegistration()).toEqual({
    success: false,
    code: 'project_registration_export_unavailable'
  })
})

it('maps an unavailable bridge or rejected IPC to a fixed code', async () => {
  window.api.database.project = {
    ...original,
    exportRegistration: undefined
  } as unknown as typeof original
  expect(await projectRegistrationAPI.exportRegistration()).toEqual({
    success: false,
    code: 'project_registration_export_unavailable'
  })
  const call = bridge(null)
  call.mockRejectedValueOnce(new Error('fixture-secret /private/path'))
  expect(await projectRegistrationAPI.exportRegistration()).toEqual({
    success: false,
    code: 'project_registration_export_unavailable'
  })
})
