use rusqlite::Connection;
use std::{
    fs,
    path::Path,
    sync::{Arc, Barrier},
    thread,
};
use uebox_chat_history::{ChatHistory, Engine, FailureCode, HistoryError, TerminalTurn, TurnState};

#[test]
fn completed_and_failed_visible_turns_survive_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("history.sqlite");
    let mut history = ChatHistory::create_new(&path).unwrap();
    history.create_chat("one", Engine::Unreal).unwrap();
    assert_eq!(
        history.create_chat("one", Engine::Godot),
        Err(HistoryError::ChatAlreadyExists)
    );
    let completed = history.begin_turn("one", "  用户\n第一轮  ").unwrap();
    history
        .finish_turn(
            completed,
            TerminalTurn::Completed {
                assistant_text: "  助手回答\n",
            },
        )
        .unwrap();
    let failed = history.begin_turn("one", "第二轮").unwrap();
    history
        .finish_turn(
            failed,
            TerminalTurn::Failed {
                code: FailureCode::TargetUnavailable,
            },
        )
        .unwrap();
    drop(history);

    let history = ChatHistory::open_existing(&path).unwrap();
    let chat = history.get_chat("one").unwrap().unwrap();
    assert_eq!(chat.summary.engine, Engine::Unreal);
    assert_eq!(chat.turns.len(), 2);
    assert_eq!(chat.turns[0].id, completed);
    assert_eq!(chat.turns[0].user_text, "  用户\n第一轮  ");
    assert_eq!(
        chat.turns[0].state,
        TurnState::Completed {
            assistant_text: "  助手回答\n".into()
        }
    );
    assert_eq!(chat.turns[1].id, failed);
    assert_eq!(chat.turns[1].user_text, "第二轮");
    assert_eq!(
        chat.turns[1].state,
        TurnState::Failed {
            code: FailureCode::TargetUnavailable
        }
    );
    assert!(chat
        .turns
        .iter()
        .all(|turn| turn.finished_at_ms.unwrap() >= turn.started_at_ms));
    assert!(chat.summary.updated_at_ms >= chat.summary.created_at_ms);
}

#[test]
fn list_is_paginated_and_the_detail_is_loaded_by_id() {
    let temp = tempfile::tempdir().unwrap();
    let mut history = ChatHistory::create_new(temp.path().join("history.sqlite")).unwrap();
    for (id, engine) in [
        ("a", Engine::Unreal),
        ("b", Engine::Godot),
        ("c", Engine::Unity),
    ] {
        history.create_chat(id, engine).unwrap();
    }
    let all = history.list_chats(10, 0).unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(history.list_chats(1, 1).unwrap(), &all[1..2]);
    assert!(history.list_chats(1, 3).unwrap().is_empty());
    assert_eq!(
        history.get_chat("b").unwrap().unwrap().summary.engine,
        Engine::Godot
    );
    assert_eq!(history.get_chat("absent").unwrap(), None);
    assert_eq!(history.list_chats(0, 0), Err(HistoryError::InvalidInput));
}

#[test]
fn no_clobber_creation_and_fail_closed_open_preserve_existing_files() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("history.sqlite");
    assert_eq!(
        ChatHistory::open_existing(&path).unwrap_err(),
        HistoryError::Missing
    );
    assert!(!path.exists());
    fs::write(&path, b"not a database").unwrap();
    assert_eq!(
        ChatHistory::open_existing(&path).unwrap_err(),
        HistoryError::Corrupt
    );
    assert_eq!(
        ChatHistory::create_new(&path).unwrap_err(),
        HistoryError::AlreadyExists
    );
    assert_eq!(fs::read(&path).unwrap(), b"not a database");

    let orphan = temp.path().join("orphan.sqlite");
    let sidecar = temp.path().join("orphan.sqlite-journal");
    fs::write(&sidecar, b"preserve this journal").unwrap();
    assert_eq!(
        ChatHistory::create_new(&orphan).unwrap_err(),
        HistoryError::AlreadyExists
    );
    assert_eq!(
        ChatHistory::open_existing(&orphan).unwrap_err(),
        HistoryError::RecoveryRequired
    );
    assert!(!orphan.exists());
    assert_eq!(fs::read(sidecar).unwrap(), b"preserve this journal");
}

fn changed_library(path: &Path, sql: &str) -> Vec<u8> {
    drop(ChatHistory::create_new(path).unwrap());
    let connection = Connection::open(path).unwrap();
    connection.execute_batch(sql).unwrap();
    drop(connection);
    fs::read(path).unwrap()
}

#[test]
fn wrong_application_version_or_exact_schema_is_refused_without_updates() {
    let temp = tempfile::tempdir().unwrap();
    for (file, sql) in [
        ("app.sqlite", "PRAGMA application_id = 1;"),
        ("version.sqlite", "PRAGMA user_version = 2;"),
        ("extra.sqlite", "CREATE TABLE unrelated(value TEXT);"),
        ("missing-index.sqlite", "DROP INDEX one_active_turn;"),
        (
            "missing-actor-receipts.sqlite",
            "DROP TABLE actor_label_receipts;",
        ),
    ] {
        let path = temp.path().join(file);
        let before = changed_library(&path, sql);
        assert_eq!(
            ChatHistory::open_existing(&path).unwrap_err(),
            HistoryError::Unsupported
        );
        assert_eq!(fs::read(&path).unwrap(), before);
    }
}

#[test]
fn competing_creators_do_not_replace_the_winner() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("history.sqlite");
    let barrier = Arc::new(Barrier::new(2));
    let results = thread::scope(|scope| {
        let jobs: Vec<_> = ["left", "right"]
            .into_iter()
            .map(|id| {
                let barrier = barrier.clone();
                let path = &path;
                scope.spawn(move || {
                    barrier.wait();
                    let mut history = ChatHistory::create_new(path)?;
                    history.create_chat(id, Engine::Unreal)?;
                    Ok::<_, HistoryError>(id)
                })
            })
            .collect();
        jobs.into_iter()
            .map(|job| job.join().unwrap())
            .collect::<Vec<_>>()
    });
    let winner = results
        .iter()
        .find_map(|result| result.as_ref().ok())
        .unwrap();
    assert_eq!(
        results
            .iter()
            .filter(|result| **result == Err(HistoryError::AlreadyExists))
            .count(),
        1
    );
    let history = ChatHistory::open_existing(&path).unwrap();
    let chats = history.list_chats(10, 0).unwrap();
    assert_eq!(chats.len(), 1);
    assert_eq!(&chats[0].id, winner);
}

#[test]
fn independent_connections_admit_one_turn_per_chat() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("history.sqlite");
    let mut first = ChatHistory::create_new(&path).unwrap();
    first.create_chat("other", Engine::Unity).unwrap();
    let second = ChatHistory::open_existing(&path).unwrap();
    let barrier = Barrier::new(2);
    let [first, second] = thread::scope(|scope| {
        [first, second]
            .map(|mut connection| {
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    connection.begin_chat_turn("same", Engine::Unreal, "concurrent")
                })
            })
            .map(|job| job.join().unwrap())
    });
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    assert!([first, second].contains(&Err(HistoryError::ActiveTurn)));
    let mut history = ChatHistory::open_existing(&path).unwrap();
    assert!(history.begin_turn("other", "independent").is_ok());
    assert_eq!(history.get_chat("same").unwrap().unwrap().turns.len(), 1);
    assert_eq!(history.get_chat("other").unwrap().unwrap().turns.len(), 1);
}

#[test]
fn concurrent_finish_and_late_duplicate_preserve_the_first_terminal_state() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("history.sqlite");
    let mut first = ChatHistory::create_new(&path).unwrap();
    first.create_chat("chat", Engine::Godot).unwrap();
    let id = first.begin_turn("chat", "visible").unwrap();
    let second = ChatHistory::open_existing(&path).unwrap();
    let barrier = Barrier::new(2);
    let results = thread::scope(|scope| {
        [first, second]
            .into_iter()
            .zip([
                TerminalTurn::Completed {
                    assistant_text: "winner",
                },
                TerminalTurn::Cancelled,
            ])
            .map(|(mut connection, terminal)| {
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    connection.finish_turn(id, terminal)
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|job| job.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert!(results.contains(&Err(HistoryError::TurnAlreadyFinished)));
    let mut history = ChatHistory::open_existing(&path).unwrap();
    let expected = if results[0].is_ok() {
        TurnState::Completed {
            assistant_text: "winner".into(),
        }
    } else {
        TurnState::Cancelled
    };
    assert_eq!(
        history.finish_turn(id, TerminalTurn::Interrupted),
        Err(HistoryError::TurnAlreadyFinished)
    );
    assert_eq!(
        history.get_chat("chat").unwrap().unwrap().turns[0].state,
        expected
    );
    assert_eq!(
        history.finish_turn(99, TerminalTurn::Cancelled),
        Err(HistoryError::TurnNotFound)
    );
}

#[test]
fn recovery_is_explicit_and_only_interrupts_persisted_active_turns() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("history.sqlite");
    let mut history = ChatHistory::create_new(&path).unwrap();
    history.create_chat("one", Engine::Unreal).unwrap();
    history.create_chat("two", Engine::Unity).unwrap();
    let previous = history.begin_turn("one", "completed user").unwrap();
    history
        .finish_turn(previous, TerminalTurn::Cancelled)
        .unwrap();
    let first = history.begin_turn("one", "keep this text").unwrap();
    history.begin_turn("two", "keep this too").unwrap();
    drop(history);

    let mut history = ChatHistory::open_existing(&path).unwrap();
    assert_eq!(
        history.get_chat("one").unwrap().unwrap().turns[1].state,
        TurnState::Active
    );
    assert_eq!(
        history.begin_turn("one", "cannot overlap"),
        Err(HistoryError::ActiveTurn)
    );
    assert_eq!(history.recover_interrupted().unwrap(), 2);
    assert_eq!(history.recover_interrupted().unwrap(), 0);
    assert_eq!(
        history.finish_turn(
            first,
            TerminalTurn::Completed {
                assistant_text: "late reply"
            }
        ),
        Err(HistoryError::TurnAlreadyFinished)
    );
    drop(history);

    let mut history = ChatHistory::open_existing(&path).unwrap();
    let turns = history.get_chat("one").unwrap().unwrap().turns;
    assert_eq!(turns[0].state, TurnState::Cancelled);
    assert_eq!(turns[1].state, TurnState::Interrupted);
    assert_eq!(turns[1].user_text, "keep this text");
    assert!(history.begin_turn("one", "after recovery").unwrap() > first);
    assert_eq!(
        history.get_chat("two").unwrap().unwrap().turns[0].state,
        TurnState::Interrupted
    );
}

#[test]
fn invalid_inputs_do_not_take_up_the_active_turn_slot() {
    let temp = tempfile::tempdir().unwrap();
    let mut history = ChatHistory::create_new(temp.path().join("history.sqlite")).unwrap();
    assert_eq!(
        history.create_chat("../bad", Engine::Unreal),
        Err(HistoryError::InvalidInput)
    );
    assert_eq!(
        history.begin_turn("missing", "text"),
        Err(HistoryError::ChatNotFound)
    );
    history.create_chat("chat", Engine::Unity).unwrap();
    assert_eq!(
        history.begin_turn("chat", "   "),
        Err(HistoryError::InvalidInput)
    );
    let id = history.begin_turn("chat", "real text").unwrap();
    assert_eq!(
        history.finish_turn(
            id,
            TerminalTurn::Completed {
                assistant_text: "\u{0}"
            }
        ),
        Err(HistoryError::InvalidInput)
    );
    assert_eq!(
        history.get_chat("chat").unwrap().unwrap().turns[0].state,
        TurnState::Active
    );
    history.finish_turn(id, TerminalTurn::Interrupted).unwrap();
}

#[test]
fn atomic_begin_creates_the_chat_and_first_title_and_refuses_a_different_engine() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("history.sqlite");
    let mut history = ChatHistory::create_new(&path).unwrap();
    let tail = "字".repeat(100);
    let text = format!("  Project\n\t preview\u{2003}{tail}  ");
    let id = history
        .begin_chat_turn("ue_01-uuid", Engine::Unreal, &text)
        .unwrap();
    let chat = history.get_chat("ue_01-uuid").unwrap().unwrap();
    let expected = format!("Project preview {}", "字".repeat(64));
    assert_eq!(chat.summary.title, expected);
    assert_eq!(chat.summary.title.chars().count(), 80);
    assert_eq!(chat.turns[0].user_text, text);
    assert_eq!(chat.turns[0].id, id);
    assert_eq!(
        history.begin_chat_turn("ue_01-uuid", Engine::Unity, "other"),
        Err(HistoryError::EngineMismatch)
    );
    history.finish_turn(id, TerminalTurn::Cancelled).unwrap();
    let second = history
        .begin_chat_turn("ue_01-uuid", Engine::Unreal, "do not replace the title")
        .unwrap();
    history
        .finish_turn(second, TerminalTurn::Interrupted)
        .unwrap();

    assert!(history
        .create_chat("empty", Engine::Godot)
        .unwrap()
        .title
        .is_empty());
    assert!(history
        .get_chat("empty")
        .unwrap()
        .unwrap()
        .summary
        .title
        .is_empty());
    history
        .begin_turn("empty", "  first\t existing   ")
        .unwrap();
    drop(history);

    let history = ChatHistory::open_existing(&path).unwrap();
    let chats = history.list_chats(10, 0).unwrap();
    let find = |id: &str| chats.iter().find(|chat| chat.id == id).unwrap();
    assert_eq!(find("ue_01-uuid").title, expected);
    assert_eq!(find("empty").title, "first existing");
}

#[test]
fn a_failed_first_turn_leaves_no_partial_chat() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("history.sqlite");
    let mut history = ChatHistory::create_new(&path).unwrap();
    assert_eq!(
        history.begin_chat_turn("blank", Engine::Unreal, ""),
        Err(HistoryError::InvalidInput)
    );
    assert_eq!(history.get_chat("blank").unwrap(), None);
    {
        let fixture = Connection::open(&path).unwrap();
        fixture
            .execute_batch(
                "CREATE TRIGGER fixture_fail_turn BEFORE INSERT ON turns
             BEGIN SELECT RAISE(ABORT, 'fixture abort'); END;",
            )
            .unwrap();
    }
    assert!(history
        .begin_chat_turn("rolled-back", Engine::Unreal, "visible")
        .is_err());
    assert_eq!(history.get_chat("rolled-back").unwrap(), None);
}

#[test]
fn assistant_text_preserves_empty_and_whitespace_answers_and_accepts_four_mib() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("history.sqlite");
    let mut history = ChatHistory::create_new(&path).unwrap();
    history.create_chat("chat", Engine::Unreal).unwrap();
    let limit = 4 * 1024 * 1024;
    let large = "€".repeat(limit / 3) + "x";
    assert_eq!(large.len(), limit);
    let too_large = large.clone() + "x";
    for assistant_text in ["", " \n\t\u{2003} ", large.as_str()] {
        let turn = history.begin_turn("chat", "question").unwrap();
        assert_eq!(
            history.finish_turn(
                turn,
                TerminalTurn::Completed {
                    assistant_text: &too_large
                }
            ),
            Err(HistoryError::InvalidInput)
        );
        history
            .finish_turn(turn, TerminalTurn::Completed { assistant_text })
            .unwrap();
    }
    drop(history);
    let history = ChatHistory::open_existing(&path).unwrap();
    let chat = history.get_chat("chat").unwrap().unwrap();
    for (turn, expected) in chat
        .turns
        .iter()
        .zip(["", " \n\t\u{2003} ", large.as_str()])
    {
        assert_eq!(
            turn.state,
            TurnState::Completed {
                assistant_text: expected.into()
            }
        );
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
const HOT_JOURNAL_FIXTURE: &str = "UEBOX_TEST_HISTORY_HOT_JOURNAL_PATH";

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn hot_journal_fixture_child() {
    let Some(path) = std::env::var_os(HOT_JOURNAL_FIXTURE) else {
        return;
    };
    let path = std::path::PathBuf::from(path);
    assert_eq!(path.file_name().unwrap(), "history.sqlite");
    assert!(path
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("uebox-chat-history-recovery-"));
    let connection =
        Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE).unwrap();
    connection.execute_batch(
        "PRAGMA cache_size = 2; PRAGMA cache_spill = ON; PRAGMA synchronous = FULL;
         BEGIN IMMEDIATE;
         UPDATE turns SET assistant_text = replace(assistant_text, 'a', 'b') WHERE state = 'completed';"
    ).unwrap();
    // Skip SQLite's normal rollback on connection drop.
    std::process::exit(0);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn exclusive_open_replays_only_a_private_single_database_hot_journal() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::Builder::new()
        .prefix("uebox-chat-history-recovery-")
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let path = temp.path().join("history.sqlite");
    let journal = temp.path().join("history.sqlite-journal");
    let mut history = ChatHistory::create_new(&path).unwrap();
    let original_answer = "a".repeat(8 * 1024);
    for _ in 0..20 {
        let turn = history
            .begin_chat_turn("ue", Engine::Unreal, "committed user text")
            .unwrap();
        history
            .finish_turn(
                turn,
                TerminalTurn::Completed {
                    assistant_text: &original_answer,
                },
            )
            .unwrap();
    }
    history
        .begin_turn("ue", "persisted before restart")
        .unwrap();
    drop(history);
    let committed_bytes = fs::read(&path).unwrap();

    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "hot_journal_fixture_child"])
        .env(HOT_JOURNAL_FIXTURE, &path)
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "{}",
        String::from_utf8_lossy(&child.stdout)
    );
    let pending_bytes = fs::read(&path).unwrap();
    let saved_journal = fs::read(&journal).unwrap();
    assert_ne!(
        pending_bytes, committed_bytes,
        "child must have spilled uncommitted pages"
    );
    assert_eq!(
        &saved_journal[..8],
        &[0xd9, 0xd5, 5, 0xf9, 0x20, 0xa1, 0x63, 0xd7]
    );
    assert_eq!(
        ChatHistory::open_existing(&path).unwrap_err(),
        HistoryError::RecoveryRequired
    );
    assert_eq!(fs::read(&journal).unwrap(), saved_journal);

    fs::set_permissions(&journal, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        ChatHistory::open_existing_recovering(&path).unwrap_err(),
        HistoryError::RecoveryRequired
    );
    fs::set_permissions(&journal, fs::Permissions::from_mode(0o600)).unwrap();

    let mut unsupported_trailer = saved_journal.clone();
    unsupported_trailer.extend_from_slice(&saved_journal[..8]);
    fs::write(&journal, &unsupported_trailer).unwrap();
    assert_eq!(
        ChatHistory::open_existing_recovering(&path).unwrap_err(),
        HistoryError::RecoveryRequired
    );
    assert_eq!(fs::read(&journal).unwrap(), unsupported_trailer);
    assert_eq!(fs::read(&path).unwrap(), pending_bytes);
    fs::write(&journal, &saved_journal).unwrap();

    let mut foreign_header = pending_bytes.clone();
    foreign_header[68..72].copy_from_slice(&1u32.to_be_bytes());
    fs::write(&path, &foreign_header).unwrap();
    assert_eq!(
        ChatHistory::open_existing_recovering(&path).unwrap_err(),
        HistoryError::Unsupported
    );
    assert_eq!(fs::read(&path).unwrap(), foreign_header);
    assert_eq!(fs::read(&journal).unwrap(), saved_journal);
    fs::write(&path, &pending_bytes).unwrap();

    let mut history = ChatHistory::open_existing_recovering(&path).unwrap();
    assert!(!journal.exists());
    let chat = history.get_chat("ue").unwrap().unwrap();
    assert!(chat.turns[..20].iter().all(|turn| turn.state
        == TurnState::Completed {
            assistant_text: original_answer.clone()
        }));
    assert_eq!(chat.turns[20].state, TurnState::Active);
    assert_eq!(history.recover_interrupted().unwrap(), 1);
}

#[test]
fn actor_label_receipts_survive_a_cancelled_turn_and_reopen() {
    use uebox_chat_history::{ActorLabelReceipt, ActorLabelStatus};

    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("history.sqlite");
    let mut history = ChatHistory::create_new(&path).unwrap();
    let turn = history
        .begin_chat_turn("ue", Engine::Unreal, "rename selected actor")
        .unwrap();
    history.finish_turn(turn, TerminalTurn::Cancelled).unwrap();
    let before = history.get_chat("ue").unwrap().unwrap().turns.remove(0);
    assert!(before.actor_label_receipts.is_empty());

    history
        .record_actor_label_receipt(turn, "42", ActorLabelStatus::Applied, Some("门_A"))
        .unwrap();
    history
        .record_actor_label_receipt(turn, "43", ActorLabelStatus::Unknown, None)
        .unwrap();
    drop(history);

    let history = ChatHistory::open_existing(&path).unwrap();
    let after = history.get_chat("ue").unwrap().unwrap().turns.remove(0);
    assert_eq!(after.state, TurnState::Cancelled);
    assert_eq!(after.finished_at_ms, before.finished_at_ms);
    assert_eq!(after.user_text, before.user_text);
    assert_eq!(
        after.actor_label_receipts,
        [
            ActorLabelReceipt {
                approval_id: "42".into(),
                status: ActorLabelStatus::Applied,
                actual_label: Some("门_A".into()),
            },
            ActorLabelReceipt {
                approval_id: "43".into(),
                status: ActorLabelStatus::Unknown,
                actual_label: None,
            },
        ]
    );
}

#[test]
fn actor_label_receipts_retry_idempotently_and_reject_a_conflicting_value() {
    use uebox_chat_history::ActorLabelStatus;

    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("history.sqlite");
    let mut history = ChatHistory::create_new(&path).unwrap();
    let first = history
        .begin_chat_turn("ue", Engine::Unreal, "request")
        .unwrap();
    history
        .record_actor_label_receipt(first, "5", ActorLabelStatus::Mismatch, Some("Actual"))
        .unwrap();
    let mut second_connection = ChatHistory::open_existing(&path).unwrap();
    second_connection
        .record_actor_label_receipt(first, "5", ActorLabelStatus::Mismatch, Some("Actual"))
        .unwrap();
    assert_eq!(
        second_connection.record_actor_label_receipt(
            first,
            "5",
            ActorLabelStatus::Mismatch,
            Some("Different")
        ),
        Err(HistoryError::ReceiptConflict)
    );
    assert_eq!(
        second_connection.record_actor_label_receipt(
            first,
            "5",
            ActorLabelStatus::Applied,
            Some("Actual")
        ),
        Err(HistoryError::ReceiptConflict)
    );
    history.finish_turn(first, TerminalTurn::Cancelled).unwrap();
    let second = history.begin_turn("ue", "next").unwrap();
    history
        .record_actor_label_receipt(second, "5", ActorLabelStatus::NotExecuted, None)
        .unwrap();
    let chat = history.get_chat("ue").unwrap().unwrap();
    assert_eq!(chat.turns[0].actor_label_receipts.len(), 1);
    assert_eq!(
        chat.turns[0].actor_label_receipts[0]
            .actual_label
            .as_deref(),
        Some("Actual")
    );
    assert_eq!(
        chat.turns[1].actor_label_receipts[0].status,
        ActorLabelStatus::NotExecuted
    );
}

#[test]
fn actor_label_receipts_refuse_invalid_fields_and_other_engines() {
    use uebox_chat_history::ActorLabelStatus::{Applied, Mismatch, NotExecuted, Unknown};

    let temp = tempfile::tempdir().unwrap();
    let mut history = ChatHistory::create_new(temp.path().join("history.sqlite")).unwrap();
    let unreal = history
        .begin_chat_turn("ue", Engine::Unreal, "first")
        .unwrap();
    let unity = history
        .begin_chat_turn("unity", Engine::Unity, "first")
        .unwrap();
    let godot = history
        .begin_chat_turn("godot", Engine::Godot, "first")
        .unwrap();
    for other_engine in [unity, godot] {
        assert_eq!(
            history.record_actor_label_receipt(other_engine, "1", Unknown, None),
            Err(HistoryError::EngineMismatch)
        );
    }
    assert_eq!(
        history.record_actor_label_receipt(999, "1", Unknown, None),
        Err(HistoryError::TurnNotFound)
    );
    let oversize = "x".repeat(4097);
    for (id, status, actual) in [
        ("", Unknown, None),
        ("contains/path", Unknown, None),
        ("1", Applied, None),
        ("2", Mismatch, None),
        ("3", NotExecuted, Some("unexpected")),
        ("4", Unknown, Some("unexpected")),
        ("5", Applied, Some("unsafe\u{0}label")),
        ("6", Mismatch, Some("unsafe\u{85}label")),
        ("7", Mismatch, Some(oversize.as_str())),
    ] {
        assert_eq!(
            history.record_actor_label_receipt(unreal, id, status, actual),
            Err(HistoryError::InvalidInput)
        );
    }
    let chat = history.get_chat("ue").unwrap().unwrap();
    assert!(chat.turns[0].actor_label_receipts.is_empty());
}
