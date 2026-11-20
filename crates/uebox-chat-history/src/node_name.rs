use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use crate::{validate_id, ActorLabelStatus, ChatHistory, Engine, HistoryError, Turn, TurnId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeNameReceipt {
    pub approval_id: String,
    pub status: ActorLabelStatus,
    pub actual_name: Option<String>,
}

impl ChatHistory {
    /// Record one Unity or Godot editor read-back against that engine's saved turn.
    /// An identical retry is safe, including after the turn has reached a terminal state.
    pub fn record_node_name_receipt(
        &mut self,
        turn_id: TurnId,
        engine: Engine,
        approval_id: &str,
        status: ActorLabelStatus,
        actual_name: Option<&str>,
    ) -> Result<(), HistoryError> {
        validate_id(approval_id)?;
        if turn_id <= 0 || !status.accepts_label(actual_name) {
            return Err(HistoryError::InvalidInput);
        }
        if engine == Engine::Unreal {
            return Err(HistoryError::EngineMismatch);
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored_engine: String = transaction.query_row(
            "SELECT chats.engine FROM turns JOIN chats ON chats.id = turns.chat_id WHERE turns.id = ?1",
            [turn_id], |row| row.get(0),
        ).optional()?.ok_or(HistoryError::TurnNotFound)?;
        if Engine::from_str(&stored_engine)? != engine {
            return Err(HistoryError::EngineMismatch);
        }
        let inserted = transaction.execute(
            "INSERT INTO node_name_receipts(turn_id, approval_id, status, actual_name)
             VALUES (?1, ?2, ?3, ?4) ON CONFLICT(turn_id, approval_id) DO NOTHING",
            params![turn_id, approval_id, status.as_str(), actual_name],
        )?;
        if inserted == 0 {
            let stored: (String, Option<String>) = transaction.query_row(
                "SELECT status, actual_name FROM node_name_receipts WHERE turn_id = ?1 AND approval_id = ?2",
                params![turn_id, approval_id], |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if stored.0 != status.as_str() || stored.1.as_deref() != actual_name {
                return Err(HistoryError::ReceiptConflict);
            }
        }
        transaction.commit()?;
        Ok(())
    }
}

pub(super) fn load_receipts(
    connection: &Connection,
    chat_id: &str,
    engine: Engine,
    turns: &mut [Turn],
) -> Result<(), HistoryError> {
    let mut statement = connection.prepare(
        "SELECT receipts.turn_id, receipts.approval_id, receipts.status, receipts.actual_name
         FROM turns JOIN node_name_receipts AS receipts ON receipts.turn_id = turns.id
         WHERE turns.chat_id = ?1 ORDER BY receipts.turn_id, receipts.approval_id",
    )?;
    let mut rows = statement.query([chat_id])?;
    while let Some(row) = rows.next()? {
        let turn_id: TurnId = row.get(0)?;
        let approval_id: String = row.get(1)?;
        let status = ActorLabelStatus::from_db(
            row.get_ref(2)?
                .as_str()
                .map_err(|_| HistoryError::Corrupt)?,
        )?;
        let actual_name: Option<String> = row.get(3)?;
        if engine == Engine::Unreal
            || validate_id(&approval_id).is_err()
            || !status.accepts_label(actual_name.as_deref())
        {
            return Err(HistoryError::Corrupt);
        }
        let index = turns
            .binary_search_by_key(&turn_id, |turn| turn.id)
            .map_err(|_| HistoryError::Corrupt)?;
        turns[index].node_name_receipts.push(NodeNameReceipt {
            approval_id,
            status,
            actual_name,
        });
    }
    Ok(())
}
