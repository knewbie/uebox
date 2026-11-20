import { invoke } from '@tauri-apps/api/core'
import { z } from 'zod'

const statusSchema = z
  .object({
    sessionId: z
      .string()
      .regex(/^[1-9][0-9]{0,19}$/)
      .nullable(),
    open: z.boolean(),
    opening: z.boolean(),
    address: z.string().nullable(),
    origin: z.string().nullable(),
    loading: z.boolean(),
    canGoBack: z.boolean(),
    canGoForward: z.boolean(),
    historySupported: z.boolean(),
    navigationBlocked: z.boolean(),
    downloadBlocked: z.boolean(),
    downloadsEnabled: z.literal(false)
  })
  .refine(
    (value) =>
      !value.open ||
      (value.sessionId !== null &&
        (value.opening || value.address !== null) &&
        value.origin !== null)
  )

export type BrowserStatus = z.infer<typeof statusSchema>
export type BrowserAction = 'back' | 'forward' | 'reload'

// Errors are handled by the panel and mapped to fixed bilingual message keys.
export async function browserStatus(): Promise<BrowserStatus> {
  return statusSchema.parse(await invoke<unknown>('isolated_browser_status'))
}

export async function openBrowser(address: string): Promise<BrowserStatus> {
  return statusSchema.parse(await invoke<unknown>('open_browser_window', { address }))
}

export async function controlBrowser(
  sessionId: string,
  action: BrowserAction
): Promise<BrowserStatus> {
  return statusSchema.parse(
    await invoke<unknown>('control_isolated_browser', { sessionId, action })
  )
}
