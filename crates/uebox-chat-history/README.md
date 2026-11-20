# Native chat history

This standalone crate stores visible conversation text in bundled SQLite. It
does not load existing Electron files, contact a model, execute tools, or
accept credentials, internal tool receipts, prompts, or provider error bodies.
The caller must pass the same visible text it presents to the user.

## Host API

- `create_new(database)` publishes an initialized file without replacing an
  existing database or sidecar. `open_existing(database)` validates the
  existing file with a read-only connection before opening a writable handle.
  Both accept an absolute file path under an existing host-owned directory.
- `create_chat(id, engine)` creates an empty chat. IDs are opaque ASCII
  alphanumerics, underscores, and hyphens, 1–128 bytes. The engine is fixed.
- `begin_chat_turn(id, engine, user_text)` atomically creates the chat if
  needed and records an active turn. An existing chat must have the same
  engine. `begin_turn(id, user_text)` is available for an existing chat.
  Both return a database-local `TurnId` (`i64`) after committing. Keep the
  ID in trusted host state and start the run only after this commit.
- `list_chats(limit, offset)` returns `ChatSummary` entries, latest activity
  first, with IDs breaking timestamp ties. Page size is 1–100. The title
  is at most 80 Unicode scalar values from the first user message after
  collapsing whitespace. Empty chats have an empty title.
  `get_chat(id)` returns the summary and visible turns in creation order.
- `finish_turn(turn_id, terminal)` writes one terminal result. The terminal
  states are completed, failed, cancelled, and interrupted. A failed turn
  has a typed `FailureCode`. Successful assistant text allows empty or
  whitespace-only strings up to 4 MiB of UTF-8 and is preserved verbatim.
  User text requires a non-whitespace character, is limited to 1 MiB of UTF-8,
  and is also preserved verbatim. Both reject NUL.

The database enforces one active turn per chat. A late completion or competing
terminal writer cannot replace the first terminal state. Application errors
and stored failure codes expose stable strings without SQLite error payloads.

`record_actor_label_receipt(turn_id, approval_id, status, actual_label)`
stores a visible UE write receipt separately from the turn's outcome. UE turns
accept receipts after completion, failure, cancellation, or interruption.
An identical receipt for the same turn and approval ID is a no-op. A different
receipt for that key returns `ReceiptConflict`. Approval IDs use the same
opaque ASCII rules as chat IDs. Only applied and mismatch outcomes require a
label, up to 4096 UTF-8 bytes with no control characters. Not-executed and
unknown outcomes require an absent label. The table stores the turn link,
approval ID, status, and label. `get_chat` returns them in each turn's
`actor_label_receipts`, ordered lexically by approval ID.

## Exclusive startup

The host owns the directory and must prevent replacement of the database and
its sidecars. Use a private local filesystem and coordinate all access through
the same SQLite implementation. Schema checks include the application ID
(`UBCH`), version, UTF-8 encoding, exact schema, integrity, and foreign keys.
Existing WAL/SHM or incompatible files are rejected. Publication uses a
same-directory staged file and a no-clobber hard link. A crash during
publication can leave a staging directory and directory-entry durability
depends on the filesystem and platform.

`open_existing` refuses every rollback journal, leaving it untouched. With
exclusive startup ownership, `open_existing_recovering` can let SQLite replay
a hot single-database journal on macOS or Linux. The host must know the pair
came from this store and hold its directory lock throughout. The crate verifies
the private owner, mode, absence of ACLs, regular-file identity and links,
rejects super-journal references, and checks the raw database header before
SQLite is allowed to recover. It validates the full database again afterward.
Unsupported platforms and file layouts the crate cannot verify leave the
journal for explicit diagnosis. These checks do not establish file provenance
or exclude writers that disregard the host's lock and already have access.

Opening and SQLite replay leave committed active turns active. After startup
and before admitting requests, call `recover_interrupted()` while still holding
exclusive ownership. It marks all abandoned active turns interrupted in one
transaction, preserves visible user text, and leaves completed, cancelled, and
failed turns as they were. Calling it again yields zero when no turns remain
active. It never resumes a model run or an editor operation.
