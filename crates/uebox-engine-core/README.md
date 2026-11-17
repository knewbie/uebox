# Engine identity and routing

This crate contains no editor protocol or process launcher. UE, Unity and Godot
adapters own those details separately. The registry tracks one process-local
connection token for each live editor, scoped to an engine and instance ID.

An adapter first calls `EngineRegistry::open_connection` and retains the returned
token in its connection task. It calls `identify` only after receiving a
verified project identity; a pending connection is never selectable. On
disconnect it calls `close_connection` with the same token.

Project identities contain an engine kind and the canonical path of an
existing project directory. The registry never guesses which engine an editor
uses, and does not perform engine-specific project detection.

The host selects within an explicit project. An implicit selection considers
only interactive editors and fails when multiple candidates exist. An explicit
instance can select a noninteractive editor or one whose interaction mode is
unknown. Adapters must register the Unknown mode when they cannot establish it;
unknown is not reported as noninteractive. Each resulting selection pins the
connection token as well as the project; callers must use `resolve_selection`
before using it. A reconnect, a conflicting project announcement or a change
from interactive to headless invalidates relevant old selections.

For a candidate shown to a user, keep its original token and use `select_exact`
when the user selects it. Calling `select` later with only an instance ID can
select a newer connection with the same ID; that method is for a fresh lookup,
not for consuming a previously displayed candidate.

A check in this registry alone cannot prevent a time-of-check/time-of-use race
after its lock is released. The adapter must dispatch via the **same
token-bound transport** and reject or cancel work when that transport closes;
it must not look up a newer connection by instance ID after validating an old
selection. A conflicting project/engine announcement retires the token here;
the adapter must also close its underlying connection.

The token is process-local and not a persistent user identifier, an
authentication credential, or an authorization decision. Editor selection,
tool approval, permissions and transport authentication remain separate
responsibilities.
