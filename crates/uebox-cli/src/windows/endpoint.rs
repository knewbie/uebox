use super::{APP_IDENTIFIER, ENDPOINT_COMPONENT, PIPE_PREFIX};
use crate::HostError;

/// `CreateNamedPipe` accepts a name of up to 256 characters including the prefix, counted in the
/// UTF-16 code units the API actually receives.
const NAME_LIMIT: usize = 256;

/// Validates an endpoint string, returning it unchanged. Pipe names are case-insensitive in the
/// kernel, so the prefix is compared case-insensitively and nothing is rewritten: the string that
/// was checked is the string that is used.
pub(super) fn checked(endpoint: &str) -> Result<&str, HostError> {
    // `get` yields None across a character boundary, so a non-ASCII first byte is rejected
    // instead of panicking on the split.
    let name = endpoint
        .get(PIPE_PREFIX.len()..)
        .filter(|_| endpoint[..PIPE_PREFIX.len()].eq_ignore_ascii_case(PIPE_PREFIX))
        .ok_or(HostError::EndpointUnreadable)?;
    if name.is_empty()
        || name.contains('\\')
        || name.trim().is_empty()
        || name.chars().any(char::is_control)
        || endpoint.encode_utf16().count() > NAME_LIMIT
    {
        return Err(HostError::EndpointUnreadable);
    }
    Ok(endpoint)
}

/// The endpoint carries the local user's SID, so two accounts on one machine never share a name.
/// It is only a label: access is decided by the descriptor the server sets, and a name collision
/// with somebody else's pipe ends in `EndpointOccupied` or `Authentication`, never in a session.
pub(super) fn default(sid_hex: &str) -> Option<String> {
    let endpoint = format!("{PIPE_PREFIX}{APP_IDENTIFIER}.{ENDPOINT_COMPONENT}-{sid_hex}");
    Some(checked(&endpoint).ok()?.to_owned())
}
