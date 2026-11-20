use rusqlite::{params, Connection, OptionalExtension, Row, Transaction, TransactionBehavior};

use crate::{
    manifest::{self, ValidatedChat, ValidatedManifest},
    AttachmentKind, AttachmentMetadata, Availability, ChatDetail, ChatMessage, ChatSummary,
    EmbeddedAttachment, HistoryError, ImageMediaType, ImportSummary, MessageRole, MessageStatus,
};

pub(super) fn import(
    connection: &mut Connection,
    manifest: &ValidatedManifest,
) -> Result<ImportSummary, HistoryError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let report = write_manifest(&transaction, manifest)?;
    transaction.commit()?;
    Ok(report)
}

pub(super) fn write_manifest(
    transaction: &Transaction<'_>,
    manifest: &ValidatedManifest,
) -> Result<ImportSummary, HistoryError> {
    let mut report = manifest.summary;
    let mut snapshot = transaction.prepare(
        "INSERT INTO snapshots(id, sid, title, created_at, updated_at, message_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(id) DO NOTHING",
    )?;
    let mut message = transaction.prepare(
        "INSERT INTO messages(snapshot_id, position, role, text, status) VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    let mut attachment = transaction.prepare(
        "INSERT INTO attachments(snapshot_id, message_position, position, kind, availability, media_type, sha256, image_bytes)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    for chat in &manifest.chats {
        if snapshot.execute(params![
            chat.id,
            chat.sid,
            chat.title,
            chat.created_at,
            chat.updated_at,
            chat.messages.len() as i64
        ])? == 0
        {
            if !same_snapshot(transaction, chat)? {
                return Err(HistoryError::Conflict);
            }
            report.already_present += 1;
            continue;
        }

        for (number, item) in chat.messages.iter().enumerate() {
            message.execute(params![
                chat.id,
                number as i64,
                item.role.as_str(),
                item.text,
                item.status.as_str()
            ])?;
            for (index, item) in item.attachments.iter().enumerate() {
                let (kind, availability, media, sha256, bytes) = item.columns();
                attachment.execute(params![
                    chat.id,
                    number as i64,
                    index as i64,
                    kind.as_str(),
                    availability.as_str(),
                    media.map(ImageMediaType::as_str),
                    sha256,
                    bytes
                ])?;
            }
        }
        report.inserted += 1;
    }
    Ok(report)
}

// A hash finds a candidate quickly, but equality is checked against the exact
// stored fields and image bytes. A collision never discards a different chat.
fn same_snapshot(connection: &Connection, chat: &ValidatedChat) -> Result<bool, HistoryError> {
    let summary = connection.query_row(
        "SELECT id, sid, title, created_at, updated_at, message_count FROM snapshots WHERE id = ?1",
        [&chat.id],
        summary_row,
    )?;
    if summary.sid != chat.sid
        || summary.title != chat.title
        || summary.created_at != chat.created_at
        || summary.updated_at != chat.updated_at
        || summary.message_count as usize != chat.messages.len()
    {
        return Ok(false);
    }

    let mut statement = connection.prepare(
        "SELECT position, role, text, status FROM messages WHERE snapshot_id = ?1 ORDER BY position",
    )?;
    let mut rows = statement.query([&chat.id])?;
    for (position, item) in chat.messages.iter().enumerate() {
        let Some(row) = rows.next()? else {
            return Ok(false);
        };
        if row.get::<_, i64>(0)? != position as i64
            || row.get::<_, String>(1)? != item.role.as_str()
            || row.get::<_, String>(2)? != item.text
            || row.get::<_, String>(3)? != item.status.as_str()
        {
            return Ok(false);
        }
    }
    if rows.next()?.is_some() {
        return Ok(false);
    }

    let mut statement = connection.prepare(
        "SELECT message_position, position, kind, availability, media_type, sha256, image_bytes
         FROM attachments WHERE snapshot_id = ?1 ORDER BY message_position, position",
    )?;
    let mut rows = statement.query([&chat.id])?;
    for (message, item) in chat.messages.iter().enumerate() {
        for (position, item) in item.attachments.iter().enumerate() {
            let Some(row) = rows.next()? else {
                return Ok(false);
            };
            let (kind, availability, media, sha256, bytes) = item.columns();
            let stored_bytes = row
                .get_ref(6)?
                .as_blob_or_null()
                .map_err(|_| HistoryError::Corrupt)?;
            if row.get::<_, i64>(0)? != message as i64
                || row.get::<_, i64>(1)? != position as i64
                || row.get::<_, String>(2)? != kind.as_str()
                || row.get::<_, String>(3)? != availability.as_str()
                || row.get::<_, Option<String>>(4)?.as_deref() != media.map(ImageMediaType::as_str)
                || row.get::<_, Option<String>>(5)?.as_deref() != sha256
                || stored_bytes != bytes
            {
                return Ok(false);
            }
        }
    }
    Ok(rows.next()?.is_none())
}

pub(super) fn list_chats(
    connection: &Connection,
    limit: u32,
    offset: i64,
) -> Result<Vec<ChatSummary>, HistoryError> {
    let mut statement = connection.prepare(
        "SELECT id, sid, title, created_at, updated_at, message_count FROM snapshots
         ORDER BY updated_at DESC, id LIMIT ?1 OFFSET ?2",
    )?;
    let rows = statement.query_map(params![limit, offset], summary_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn summary_row(row: &Row<'_>) -> rusqlite::Result<ChatSummary> {
    Ok(ChatSummary {
        id: row.get(0)?,
        sid: row.get(1)?,
        title: row.get(2)?,
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
        message_count: row.get(5)?,
    })
}

pub(super) fn get_chat(
    connection: &Connection,
    snapshot: &str,
) -> Result<Option<ChatDetail>, HistoryError> {
    let transaction = connection.unchecked_transaction()?;
    let Some(summary) = transaction.query_row(
        "SELECT id, sid, title, created_at, updated_at, message_count FROM snapshots WHERE id = ?1",
        [snapshot], summary_row,
    ).optional()? else { return Ok(None); };

    let mut messages = Vec::with_capacity(summary.message_count as usize);
    {
        let mut statement = transaction.prepare(
            "SELECT position, role, text, status FROM messages WHERE snapshot_id = ?1 ORDER BY position",
        )?;
        let mut rows = statement.query([snapshot])?;
        while let Some(row) = rows.next()? {
            if row.get::<_, i64>(0)? != messages.len() as i64 {
                return Err(HistoryError::Corrupt);
            }
            messages.push(ChatMessage {
                role: MessageRole::from_db(&row.get::<_, String>(1)?)?,
                text: row.get(2)?,
                status: MessageStatus::from_db(&row.get::<_, String>(3)?)?,
                attachments: Vec::new(),
            });
        }
        if messages.len() != summary.message_count as usize {
            return Err(HistoryError::Corrupt);
        }
        let mut statement = transaction.prepare(
            "SELECT id, message_position, position, kind, availability, media_type
             FROM attachments WHERE snapshot_id = ?1 ORDER BY message_position, position",
        )?;
        let mut rows = statement.query([snapshot])?;
        while let Some(row) = rows.next()? {
            let index = row.get::<_, usize>(1)?;
            let message = messages.get_mut(index).ok_or(HistoryError::Corrupt)?;
            if row.get::<_, usize>(2)? != message.attachments.len() {
                return Err(HistoryError::Corrupt);
            }
            let availability = Availability::from_db(&row.get::<_, String>(4)?)?;
            let id = match availability {
                Availability::Embedded => Some(format!("a-{}", row.get::<_, i64>(0)?)),
                Availability::Unavailable => None,
            };
            let media_type = row
                .get::<_, Option<String>>(5)?
                .as_deref()
                .map(ImageMediaType::from_db)
                .transpose()?;
            message.attachments.push(AttachmentMetadata {
                kind: AttachmentKind::from_db(&row.get::<_, String>(3)?)?,
                availability,
                id,
                media_type,
            });
        }
    }
    transaction.commit()?;
    Ok(Some(ChatDetail { summary, messages }))
}

pub(super) fn get_attachment(
    connection: &Connection,
    id: i64,
) -> Result<Option<EmbeddedAttachment>, HistoryError> {
    let attachment = connection
        .query_row(
            "SELECT media_type, sha256, image_bytes FROM attachments
         WHERE id = ?1 AND availability = 'embedded' AND length(image_bytes) BETWEEN 1 AND ?2",
            params![id, manifest::MAX_IMAGE_BYTES as i64],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((media, sha256, bytes)) = attachment else {
        return Ok(None);
    };
    let media_type = ImageMediaType::from_db(&media)?;
    if !manifest::valid_image(&bytes, media_type, &sha256) {
        return Err(HistoryError::Corrupt);
    }
    Ok(Some(EmbeddedAttachment { media_type, bytes }))
}
