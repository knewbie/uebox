# Project registration library

This standalone Rust crate stores manually registered Unreal, Unity and Godot
project directories in a new, explicitly located SQLite database. It does not
discover, read, migrate or initialize the Electron project database; the host
must choose a separate private data directory. It does not open project files,
modify projects, scan for editors, or start them.

- `ProjectLibrary::create_new(absolute_database_path)` initializes a fresh
  database in a same-directory staging area and publishes it with a no-clobber
  hard link. An existing file, symlink, or SQLite sidecar is never overwritten.
- `ProjectLibrary::open_existing(absolute_database_path)` accepts this
  library's SQLite application ID and exact schema. A legacy schema-v1 library
  (the pre-metadata/cover schema) is upgraded to v2 in place, atomically, in one
  transaction, preserving every registration — IDs, engine, name, and path,
  including offline ones. Any other version, extra object, WAL-mode database or
  database hard link is rejected without modification.
- `register(engine, name, absolute_project_directory)` records the user-chosen
  path and name. The directory must be online at registration. Within the same
  engine, current online directory identities are compared to reject symlink
  aliases and other aliases of the same directory. The same directory may be
  registered separately for different engines.
- `list()` returns the stored records without accessing project directories.
  `remove(id)` deletes only the registration, even when its directory is now
  offline. IDs are not reused after deletion. Records persist across reopening.
- `import_batch(&[ImportRecord])` merges an entire batch in one transaction.
  `create_with_import(absolute_database_path, &[ImportRecord])` commits the
  initial batch to the staged database before publishing it with the same
  no-clobber rules as `create_new`. The caller supplies decoded records.

Bulk imports validate all names and native absolute UTF-8 paths for emptiness,
format, and NUL characters without accessing the project directories. Offline
records are accepted. Matching is by exact engine and stored path string, so
different spellings and aliases can coexist. An existing match keeps its name
and ID. Within the batch the first occurrence is used, and subsequent matches
count as `duplicate_in_file` even when the first was `already_registered`.
Skipped records do not use IDs. The returned summary describes a committed
batch.

## Engine-agnostic metadata

`set_metadata(project_id, note, description, is_pinned, sort_order)` stores the
user's per-project note, description, pinned flag, and sort order, keyed by the
registration ID. It is a full write: `None` (or an empty string) clears a text
field, and every field is written together in one transaction, then read back
before it is returned. `get_metadata(project_id)` returns the stored row or
`None` when it has never been set; `list_with_metadata()` returns every
registration with its metadata (defaulted) and cover metadata, ordered
pinned-first, then sort order, then registration ID. None of this reads the
project on disk — a registration with no metadata row is all defaults.

## Project covers

Covers are explicit only: the host supplies bytes from a file the user chose,
never a scan of the project. `set_cover(covers_dir, project_id, source, bytes)`
verifies the magic signature (PNG, JPEG, or WebP only, at most 32 MiB) and the
declared pixel dimensions (each side ≤ `MAX_COVER_DIMENSION`, total ≤
`MAX_COVER_PIXELS`, so a tiny compressed file cannot decode into a huge image),
writes a private copy named `<project_id>.<ext>` (ext from the format) into the
host-supplied covers directory, and records the detected format plus the source
path as provenance. The source path is stored for display and is never opened.
`get_cover` returns the stored metadata. `read_cover` returns the private copy's
bytes, but only when the stored file name matches the derived
`<project_id>.<ext>` and the copy is a regular, non-symlink file within the size
limit; a missing file reads as absent. `remove_cover` deletes both the row and,
when present, the private file. Replacing a cover with a different format
removes the stale copy. The covers directory is created with owner-only
permissions when it does not exist and must never be a symlink or reparse point.

## Boundaries

The host is responsible for a private, separate database location and must not
pass an Electron database path. File application ID and schema checks detect
incompatible databases, but are not cryptographic proof of file provenance.
The library is designed to be the sole in-process SQLite/VFS owner of its
database; do not concurrently open the same database through a different
SQLite runtime or raw-file locking code. Calls using this crate coordinate
through SQLite transactions. The caller must also prevent concurrent renames,
hard-link changes or sidecar replacement by unrelated code; filesystem paths
cannot be atomically reserved with their SQLite sidecar names as a group.

The covers directory is a separate, host-supplied location. The cover write and
the cover-metadata row are committed in two steps (file first, then row), so a
failed row write rolls the file back best-effort but the two are not a single
atomic unit. `remove(id)` cascades the metadata and cover rows in the same
transaction, but cannot reach the private copy file — call `remove_cover`
before `remove` when the file must also be deleted.

The selected project path and name are stored exactly as supplied. Paths must
be absolute and representable as UTF-8; this avoids silently replacing bytes or
unpaired UTF-16. Offline records are retained. Registration skips records whose
paths return `NotFound`, and fails when access to an existing directory cannot
be determined for another reason. A later change to symlinks or mounted volumes
can change whether previously registered paths refer to the same directory;
no automatic merge or background rescan is performed.

Only SQLite DELETE journal mode is supported. A hot rollback journal may need
recovery and can cause a read-only validation error; this crate does not repair
it. Database integrity and schema checks do not prove the provenance of files
or protect against arbitrary concurrent filesystem replacement. The database
parent must exist, support hard links and be controlled by the host. A crash can
leave `.uebox-project-library-incomplete-*` staging directories; directory-entry
durability after a power loss is not guaranteed. Unix output mode bits are set
to `0600` (staging directory `0700`), but inherited ACLs may grant additional
access. Windows inherits the parent directory's ACL, which the host must secure.

On Unix, project directories and database files are identified by device and
inode. Windows code uses the volume serial number and file index, including
SQLite's native file handle, and handles canonical Windows paths; it has not
been built or tested on Windows. Filesystems that cannot supply reliable file
identity are rejected when detected. Network filesystems and unusual path,
link or ACL semantics require platform acceptance testing.
