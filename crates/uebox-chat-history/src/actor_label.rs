use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use crate::{validate_id, ChatHistory, Engine, HistoryError, Turn, TurnId};

const MAX_LABEL_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorLabelStatus {
    Applied,
    Mismatch,
    NotExecuted,
    Unknown,
}

impl ActorLabelStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Mismatch => "mismatch",
            Self::NotExecuted => "not_executed",
            Self::Unknown => "unknown",
        }
    }

    pub(crate) fn from_db(value: &str) -> Result<Self, HistoryError> {
        match value {
            "applied" => Ok(Self::Applied),
            "mismatch" => Ok(Self::Mismatch),
            "not_executed" => Ok(Self::NotExecuted),
            "unknown" => Ok(Self::Unknown),
            _ => Err(HistoryError::Corrupt),
        }
    }

    pub(crate) fn accepts_label(self, actual_label: Option<&str>) -> bool {
        match (self, actual_label) {
            (Self::Applied | Self::Mismatch, Some(label)) => {
                label.len() <= MAX_LABEL_BYTES && !label.chars().any(char::is_control)
            }
            (Self::NotExecuted | Self::Unknown, None) => true,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorLabelReceipt {
    pub approval_id: String,
    pub status: ActorLabelStatus,
    pub actual_label: Option<String>,
}

impl ChatHistory {
    /// Record one visible write receipt, including after the turn has ended.
    /// Repeating identical data for this turn and approval succeeds without a write.
    pub fn record_actor_label_receipt(
        &mut self,
        turn_id: TurnId,
        approval_id: &str,
        status: ActorLabelStatus,
        actual_label: Option<&str>,
    ) -> Result<(), HistoryError> {
        validate_id(approval_id)?;
        if turn_id <= 0 || !status.accepts_label(actual_label) {
            return Err(HistoryError::InvalidInput);
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let engine: String = transaction
            .query_row(
                "SELECT chats.engine FROM turns JOIN chats ON chats.id = turns.chat_id
                 WHERE turns.id = ?1",
                [turn_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(HistoryError::TurnNotFound)?;
        if Engine::from_str(&engine)? != Engine::Unreal {
            return Err(HistoryError::EngineMismatch);
        }

        let inserted = transaction.execute(
            "INSERT INTO actor_label_receipts(turn_id, approval_id, status, actual_label)
             VALUES (?1, ?2, ?3, ?4) ON CONFLICT(turn_id, approval_id) DO NOTHING",
            params![turn_id, approval_id, status.as_str(), actual_label],
        )?;
        if inserted == 0 {
            let (stored_status, stored_label): (String, Option<String>) = transaction.query_row(
                "SELECT status, actual_label FROM actor_label_receipts
                 WHERE turn_id = ?1 AND approval_id = ?2",
                params![turn_id, approval_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if stored_status != status.as_str() || stored_label.as_deref() != actual_label {
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
        "SELECT receipts.turn_id, receipts.approval_id, receipts.status, receipts.actual_label
         FROM turns JOIN actor_label_receipts AS receipts ON receipts.turn_id = turns.id
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
        let actual_label: Option<String> = row.get(3)?;
        if engine != Engine::Unreal
            || validate_id(&approval_id).is_err()
            || !status.accepts_label(actual_label.as_deref())
        {
            return Err(HistoryError::Corrupt);
        }
        let index = turns
            .binary_search_by_key(&turn_id, |turn| turn.id)
            .map_err(|_| HistoryError::Corrupt)?;
        turns[index].actor_label_receipts.push(ActorLabelReceipt {
            approval_id,
            status,
            actual_label,
        });
    }
    Ok(())
}
