# SQLite inspection and snapshots

This crate is **not a general-purpose safe interface** to an active SQLite
database. In particular, independently loaded SQLite/VFS implementations and
raw POSIX locks in the same process do not necessarily share a lock manager.
Opening or closing a second SQLite instance on the same inode can release
another instance's locks and cause committed updates to be lost.

The path-only `inspect_database`, `read_projects` and `create_snapshot`
entrypoints therefore always return `ExclusiveAccessRequired` **before any
source or destination filesystem access**, without evaluating either path.
They accept borrowed arguments only, including both source and destination for
snapshots. This is an API change for callers that previously moved owned paths.
The caller must keep any wrapper holding a file descriptor alive while its
locks are needed: a temporary borrowed wrapper may still be dropped by the
caller at the end of the statement. These entrypoints do not automatically
detect other connections, VFS implementations, locks or private-copy ownership.

## Explicitly attested private copies

`unsafe PrivateSource::from_host_verified_copy(path, &mut host_guard)` is the
only opt-in entrypoint. The mutable guard binds the token's lifetime to a host
object, and the token is neither `Send` nor `Sync`. Neither the guard's type nor
its existence proves exclusivity: the host must actually establish and retain
every guarantee in the constructor's `# Safety` contract. In particular:

- The host must own a stable, private copy of the complete database file set.
  It must first stop the original writers and preserve the main database and
  existing WAL/SHM coherently; copying an active file set is not a recovery
  procedure. This crate does not create, verify or coordinate that copy.
- For the token's entire lifetime, no other SQLite/VFS connection, raw file
  handle or locking code in this process **or another process** may use the
  private source inode or sidecars. The host must also prevent path, hard-link
  and sidecar replacement. A mutex limited to this crate is not proof.
- The host must coordinate the target name and its SQLite sidecar names and
  keep the target directory and its ancestors protected by appropriate ACLs
  and traversal permissions.

If the host cannot establish those conditions, it must not construct the token;
the path-only APIs remain disabled. No Tauri integration or ownership protocol
has been implemented. The token does not make an unverified live source safe.

The token exposes `inspect()`, `read_projects()` and
`create_snapshot(destination)`. They operate in-process with a SQLite read
transaction and backup API, never initialize or migrate the source, and retain
raw project columns and SQLite storage classes. A missing `projects` table is
an error. An invalid UTF-8 project column name returns `Unsupported`; a backup
can still retain that schema. Unavailable virtual-table modules are reported;
if `quick_check` itself requires an unavailable tokenizer or module, the
operation fails rather than claiming complete validation.

## Other boundaries

Only Unix VFS implementations supporting `SQLITE_FCNTL_HAS_MOVED` are
currently supported. No operation can succeed on Windows or other non-Unix
platforms, even with host attestation. Source or destination input checks may
first return `Missing`, `Io` or another error before identity verification
returns `Unsupported` for an otherwise suitable source. Windows/Linux have not
been tested. Source
databases with multiple hard links, and sidecars that are symlinks, non-regular
files or have multiple hard links, are rejected. Identity checks cannot detect
all transient replacement races and do not replace host coordination.

SQLite may create an empty source `-wal` and create or change `-shm`; a WAL
database without these files may need a writable source directory. Use a stable
private copy if the original file set must remain untouched. SQLite checksums,
WAL salts and `quick_check` cannot prove that a WAL belongs to a particular main
database, that a missing WAL never contained a commit, or that a valid-boundary
tail was not lost. Different-salt WAL frames are rejected as ambiguous.

Snapshot publication uses a same-filesystem, no-clobber hard link after the
staged database is closed, checked and synced. Destination names ending in
`-wal`, `-shm` or `-journal` are reserved; existing output files or sidecars are
rejected. Filesystems without hard-link support fail closed. Unix staging mode
bits are `0700` for the directory and `0600` for the output file, but inherited
ACLs can grant additional access even when those bits are restrictive. The
caller must secure the target directory **and its ancestors**, including ACLs;
this crate does not remove or validate inherited ACLs. A crash can leave an
`.uebox-snapshot-incomplete-*` directory, and directory-entry durability after
power loss is not guaranteed.

This is one database, not a complete application/vault/credential/attachment
backup or a migration, restore, cross-version lock or Tauri integration.
