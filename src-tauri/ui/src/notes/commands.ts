import { invoke } from '@tauri-apps/api/core'
import { z } from 'zod'

export const NOTES_PAGE_SIZE = 20
const id = z.string().regex(/^\d+$/)
const nextId = z.union([id, z.literal(''), z.null()]).transform((value) => value || null)
const notebookSchema = z.object({ id, title: z.string() })
const summarySchema = z.object({ id, title: z.string(), notebookId: id.nullable() })
const noteSchema = summarySchema.extend({ content: z.string() })
const notebooksPage = z.object({ items: z.array(notebookSchema), nextId })
const notesPage = z.object({ items: z.array(summarySchema), nextId })

export type Notebook = z.infer<typeof notebookSchema>
export type NoteSummary = z.infer<typeof summarySchema>
export type Note = z.infer<typeof noteSchema>
export type SaveNoteInput = Omit<Note, 'id'> & { id: string | null }

async function request<T>(
  command: string,
  args: Record<string, unknown>,
  parse: (value: unknown) => T
): Promise<T> {
  try {
    return parse(await invoke<unknown>(command, args))
  } catch {
    // Host errors and schema diagnostics can contain note content or private database paths.
    throw new Error('notes_unavailable')
  }
}

function checkCursor(cursor: string | null, afterId: string | null): void {
  if (cursor !== null && cursor === afterId) throw new Error('notes_unavailable')
}

export function listNotebooks(afterId: string | null): Promise<z.infer<typeof notebooksPage>> {
  return request('list_notebooks', { afterId, limit: NOTES_PAGE_SIZE }, (value) => {
    const page = notebooksPage.parse(value)
    checkCursor(page.nextId, afterId)
    return page
  })
}

export function listNotes(
  notebookId: string | null,
  afterId: string | null
): Promise<z.infer<typeof notesPage>> {
  return request('list_notes', { notebookId, afterId, limit: NOTES_PAGE_SIZE }, (value) => {
    const page = notesPage.parse(value)
    checkCursor(page.nextId, afterId)
    if (page.items.some((note) => note.notebookId !== notebookId))
      throw new Error('notes_unavailable')
    return page
  })
}

export function getNote(noteId: string): Promise<Note> {
  return request('get_note', { noteId }, (value) => {
    const note = noteSchema.parse(value)
    if (note.id !== noteId) throw new Error('notes_unavailable')
    return note
  })
}

export function saveNotebook(input: { id: string | null; title: string }): Promise<Notebook> {
  return request('save_notebook', input, (value) => {
    const notebook = notebookSchema.parse(value)
    if (input.id !== null && notebook.id !== input.id) throw new Error('notes_unavailable')
    return notebook
  })
}

export function saveNote(input: SaveNoteInput): Promise<Note> {
  return request('save_note', input, (value) => {
    const note = noteSchema.parse(value)
    if ((input.id !== null && note.id !== input.id) || note.notebookId !== input.notebookId) {
      throw new Error('notes_unavailable')
    }
    return note
  })
}

export function removeNotebook(notebookId: string): Promise<boolean> {
  return request('remove_notebook', { notebookId }, (value) => z.boolean().parse(value))
}

export function removeNote(noteId: string): Promise<boolean> {
  return request('remove_note', { noteId }, (value) => z.boolean().parse(value))
}
