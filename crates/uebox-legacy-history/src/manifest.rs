use std::{
    collections::HashSet,
    fs::{self, Metadata, OpenOptions},
    io::Read,
    marker::PhantomData,
    path::Path,
};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{
    de::{value::MapAccessDeserializer, MapAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};
use sha2::{Digest, Sha256};

use crate::{
    storage, AttachmentKind, Availability, HistoryError, ImageMediaType, ImportSummary,
    ManifestSource, MessageRole, MessageStatus,
};

const MAX_MANIFEST_BYTES: usize = 16 * 1024 * 1024;
const MAX_CHATS: usize = 4096;
const MAX_MESSAGES: usize = 100_000;
pub(super) const MAX_IMAGE_BYTES: usize = 2 * 1024 * 1024;
const MAX_ATTACHMENT_BYTES: usize = 8 * 1024 * 1024;
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

// A derived struct also accepts a positional JSON array. The export only uses objects.
#[derive(Serialize)]
#[serde(transparent)]
struct Object<T>(T);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Object<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor<T>(PhantomData<T>);

        impl<'de, T: Deserialize<'de>> Visitor<'de> for ObjectVisitor<T> {
            type Value = Object<T>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a JSON object")
            }

            fn visit_map<M: MapAccess<'de>>(self, map: M) -> Result<Self::Value, M::Error> {
                T::deserialize(MapAccessDeserializer::new(map)).map(Object)
            }
        }

        deserializer.deserialize_map(ObjectVisitor(PhantomData))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireManifest {
    format: String,
    version: u32,
    chats: Vec<Object<WireChat>>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireChat {
    id: String,
    title: String,
    created_at: i64,
    updated_at: i64,
    messages: Vec<Object<WireMessage>>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WireMessage {
    role: MessageRole,
    text: String,
    status: MessageStatus,
    #[serde(default)]
    attachments: Vec<Object<WireAttachment>>,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "availability", rename_all = "snake_case", deny_unknown_fields)]
enum WireAttachment {
    Unavailable {
        kind: AttachmentKind,
    },
    Embedded {
        kind: AttachmentKind,
        #[serde(rename = "mediaType")]
        media_type: ImageMediaType,
        sha256: String,
        base64: String,
    },
}

pub(super) struct ValidatedManifest {
    pub chats: Vec<ValidatedChat>,
    pub summary: ImportSummary,
}

pub(super) struct ValidatedChat {
    pub id: String,
    pub sid: String,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub messages: Vec<ValidatedMessage>,
}

pub(super) struct ValidatedMessage {
    pub role: MessageRole,
    pub text: String,
    pub status: MessageStatus,
    pub attachments: Vec<ValidatedAttachment>,
}

pub(super) enum ValidatedAttachment {
    Unavailable {
        kind: AttachmentKind,
    },
    Embedded {
        media_type: ImageMediaType,
        sha256: String,
        bytes: Vec<u8>,
    },
}

impl ValidatedAttachment {
    pub fn columns(
        &self,
    ) -> (
        AttachmentKind,
        Availability,
        Option<ImageMediaType>,
        Option<&str>,
        Option<&[u8]>,
    ) {
        match self {
            Self::Unavailable { kind } => (*kind, Availability::Unavailable, None, None, None),
            Self::Embedded {
                media_type,
                sha256,
                bytes,
            } => (
                AttachmentKind::Image,
                Availability::Embedded,
                Some(*media_type),
                Some(sha256),
                Some(bytes),
            ),
        }
    }
}

pub(super) fn parse(source: ManifestSource<'_>) -> Result<ValidatedManifest, HistoryError> {
    let owned;
    let bytes = match source {
        ManifestSource::Bytes(bytes) => bytes,
        ManifestSource::Path(path) => {
            owned = read_source(path)?;
            &owned
        }
    };
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(HistoryError::LimitExceeded);
    }
    let utf8 = std::str::from_utf8(bytes).map_err(|_| HistoryError::InvalidManifest)?;
    let Object(wire): Object<WireManifest> =
        serde_json::from_str(utf8).map_err(|_| HistoryError::InvalidManifest)?;
    if wire.format != "uebox-visible-chat-history" || wire.version != 1 {
        return Err(HistoryError::InvalidManifest);
    }
    if wire.chats.len() > MAX_CHATS
        || wire
            .chats
            .iter()
            .map(|Object(chat)| chat.messages.len())
            .sum::<usize>()
            > MAX_MESSAGES
    {
        return Err(HistoryError::LimitExceeded);
    }

    let mut sids = HashSet::with_capacity(wire.chats.len());
    for Object(chat) in &wire.chats {
        if chat.id.trim_matches(js_whitespace).is_empty()
            || chat.id.encode_utf16().count() > 256
            || chat.id.contains('\0')
            || !sids.insert(&chat.id)
            || !(0..=MAX_SAFE_INTEGER).contains(&chat.created_at)
            || !(0..=MAX_SAFE_INTEGER).contains(&chat.updated_at)
        {
            return Err(HistoryError::InvalidManifest);
        }
    }

    let mut budget = MAX_ATTACHMENT_BYTES;
    let mut summary = ImportSummary::default();
    let chats = wire
        .chats
        .into_iter()
        .map(|Object(chat)| {
            // Serde gives this exact v1 shape a stable order independently of input JSON formatting.
            let canonical = serde_json::to_vec(&chat).map_err(|_| HistoryError::InvalidManifest)?;
            let id = format!("s-{}", sha256_hex(&canonical));
            drop(canonical);
            let messages = chat
                .messages
                .into_iter()
                .map(|Object(message)| {
                    if message.role == MessageRole::User
                        && message.status != MessageStatus::Completed
                    {
                        return Err(HistoryError::InvalidManifest);
                    }
                    let attachments = message
                        .attachments
                        .into_iter()
                        .map(|Object(attachment)| match attachment {
                            WireAttachment::Unavailable { kind } => {
                                summary.unavailable_attachments += 1;
                                Ok(ValidatedAttachment::Unavailable { kind })
                            }
                            WireAttachment::Embedded {
                                kind,
                                media_type,
                                sha256,
                                base64,
                            } => {
                                if kind != AttachmentKind::Image || !is_sha256(&sha256) {
                                    return Err(HistoryError::InvalidManifest);
                                }
                                let bytes =
                                    decode_image(&base64, media_type, &sha256, &mut budget)?;
                                summary.embedded_attachments += 1;
                                Ok(ValidatedAttachment::Embedded {
                                    media_type,
                                    sha256,
                                    bytes,
                                })
                            }
                        })
                        .collect::<Result<_, HistoryError>>()?;
                    Ok(ValidatedMessage {
                        role: message.role,
                        text: message.text,
                        status: message.status,
                        attachments,
                    })
                })
                .collect::<Result<_, HistoryError>>()?;
            Ok(ValidatedChat {
                id,
                sid: chat.id,
                title: chat.title,
                created_at: chat.created_at,
                updated_at: chat.updated_at,
                messages,
            })
        })
        .collect::<Result<_, HistoryError>>()?;
    Ok(ValidatedManifest { chats, summary })
}

fn decode_image(
    encoded: &str,
    media_type: ImageMediaType,
    sha256: &str,
    budget: &mut usize,
) -> Result<Vec<u8>, HistoryError> {
    if encoded.is_empty() || !encoded.len().is_multiple_of(4) {
        return Err(HistoryError::InvalidManifest);
    }
    let padding = if encoded.ends_with("==") {
        2
    } else {
        usize::from(encoded.ends_with('='))
    };
    let length = encoded.len() / 4 * 3 - padding;
    if length > MAX_IMAGE_BYTES || length > *budget {
        return Err(HistoryError::LimitExceeded);
    }
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| HistoryError::InvalidManifest)?;
    if bytes.len() != length
        || STANDARD.encode(&bytes) != encoded
        || !valid_image(&bytes, media_type, sha256)
    {
        return Err(HistoryError::InvalidManifest);
    }
    *budget -= bytes.len();
    Ok(bytes)
}

pub(super) fn valid_image(bytes: &[u8], media_type: ImageMediaType, sha256: &str) -> bool {
    let matches_magic = match media_type {
        ImageMediaType::Png => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        ImageMediaType::Jpeg => bytes.starts_with(b"\xff\xd8\xff"),
        ImageMediaType::Gif => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        ImageMediaType::Webp => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
    };
    matches_magic && sha256_hex(bytes) == sha256
}

pub(super) fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        text.push(char::from(HEX[(byte >> 4) as usize]));
        text.push(char::from(HEX[(byte & 0xf) as usize]));
    }
    text
}

// ECMAScript String.trim's whitespace and line terminators, as used by the exporter.
fn js_whitespace(character: char) -> bool {
    matches!(character, '\t'..='\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}'
        | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}

fn read_source(path: &Path) -> Result<Vec<u8>, HistoryError> {
    if !path.is_absolute() {
        return Err(HistoryError::InvalidInput);
    }
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        if !matches!(
            path.components().next(),
            Some(Component::Prefix(prefix))
                if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
        ) {
            return Err(HistoryError::InvalidInput);
        }
    }
    let before = fs::symlink_metadata(path).map_err(storage::io_error)?;
    if !storage::regular_file(&before) {
        return Err(HistoryError::InvalidInput);
    }
    if before.len() > MAX_MANIFEST_BYTES as u64 {
        return Err(HistoryError::LimitExceeded);
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let mut file = options.open(path).map_err(storage::io_error)?;
    let opened = file.metadata().map_err(storage::io_error)?;
    if !same_source(&before, &opened)? {
        return Err(HistoryError::InvalidInput);
    }
    let mut bytes = Vec::with_capacity(opened.len() as usize);
    (&mut file)
        .take((MAX_MANIFEST_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(storage::io_error)?;
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(HistoryError::LimitExceeded);
    }
    if bytes.len() as u64 != opened.len()
        || !same_source(&opened, &file.metadata().map_err(storage::io_error)?)?
    {
        return Err(HistoryError::InvalidInput);
    }
    Ok(bytes)
}

fn same_source(left: &Metadata, right: &Metadata) -> Result<bool, HistoryError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if left.dev() != right.dev()
            || left.ino() != right.ino()
            || left.ctime() != right.ctime()
            || left.ctime_nsec() != right.ctime_nsec()
        {
            return Ok(false);
        }
    }
    Ok(storage::regular_file(right)
        && left.len() == right.len()
        && left.modified().map_err(storage::io_error)?
            == right.modified().map_err(storage::io_error)?)
}
