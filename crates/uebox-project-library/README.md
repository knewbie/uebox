# Project registration library

This standalone Rust crate stores manually registered Unreal, Unity and Godot
project directories in a new, explicitly located SQLite database. It does not
discover, read, migrate or initialize the Electron project database; the host
must choose a separate private data directory. It does not open project files,
modify projects, scan for editors, or start them.

- `ProjectLibrary::create_new(absolute_database_path)` initializes a fresh
  database in a same-directory staging area and publishes it with a no-clobber
  hard link. An existing file, symlink, or SQLite sidecar is never overwritten.
- `ProjectLibrary::open_existing(absolute_database_path)` accepts only this
  library's SQLite application ID, version and exact schema. Unknown versions,
  extra objects, WAL-mode databases and database hard links are rejected; the
  library never auto-clears or migrates them.
- `register(engine, name, absolute_project_directory)` records the user-chosen
  path and name. The directory must be online at registration. Within the same
  engine, current online directory identities are compared to reject symlink
  aliases and other aliases of the same directory. The same directory may be
  registered separately for different engines.
- `list()` returns the stored records without accessing project directories.
  `remove(id)` deletes only the registration, even when its directory is now
  offline. IDs are not reused after deletion. Records persist across reopening.

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
