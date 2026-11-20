use std::io::{self, Write};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::{AskReply, Capabilities, HostError, OnlineProject, ReadTool, ToolReply};

pub(super) const MAX_REQUEST: usize = 128 * 1024;
pub(super) const MAX_RESPONSE: usize = 1024 * 1024;

// Identical frames to `crate::unix::wire`, so a host cannot tell which transport reached it and
// one contract covers both. It is a separate module because the Unix reader is bound to
// `UnixStream`; these readers and writers are generic over the pipe halves instead.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Request {
    Hello {
        version: u32,
    },
    Projects,
    Tools,
    PrepareRead {
        tool: ReadTool,
        #[serde(rename = "connectionId")]
        connection_id: String,
    },
    ExecuteRead,
    PrepareAsk {
        #[serde(rename = "projectId")]
        project_id: String,
        #[serde(rename = "modelProfileId")]
        model_profile_id: String,
        question: String,
    },
    ExecuteAsk,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Response {
    Hello { capabilities: Capabilities },
    Projects { projects: Vec<OnlineProject> },
    Tools { tools: Vec<ReadTool> },
    Ready,
    Read { receipt: ToolReply },
    Ask { reply: AskReply },
    Failure { error: HostError, dispatched: bool },
}

impl Response {
    pub(super) fn rejected(error: HostError) -> Self {
        Self::Failure {
            error,
            dispatched: false,
        }
    }
}

pub(super) async fn read<T: DeserializeOwned, R: AsyncRead + Unpin + ?Sized>(
    pipe: &mut R,
    limit: usize,
) -> io::Result<T> {
    let size = pipe.read_u32().await? as usize;
    if size == 0 || size > limit {
        return Err(invalid_data());
    }
    let mut bytes = vec![0; size];
    pipe.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes).map_err(|_| invalid_data())
}

struct Buffer {
    bytes: Vec<u8>,
    limit: usize,
}

impl Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit - self.bytes.len() {
            return Err(invalid_data());
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) async fn write<W: AsyncWrite + Unpin + ?Sized>(
    pipe: &mut W,
    value: &impl Serialize,
    limit: usize,
) -> io::Result<()> {
    let mut buffer = Buffer {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut buffer, value).map_err(|_| invalid_data())?;
    pipe.write_u32(buffer.bytes.len() as u32).await?;
    pipe.write_all(&buffer.bytes).await
}

// The same signal the Unix side uses: InvalidData means "the peer is not speaking this contract",
// which the caller reports as Incompatible instead of a transport outage.
fn invalid_data() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
}
