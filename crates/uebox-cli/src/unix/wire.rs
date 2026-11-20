use std::io::{self, Write};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};

use crate::{AskReply, Capabilities, HostError, OnlineProject, ReadTool, ToolReply};

pub(super) const MAX_REQUEST: usize = 128 * 1024;
pub(super) const MAX_RESPONSE: usize = 1024 * 1024;

#[derive(Deserialize, Serialize)]
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

pub(super) async fn read<T: DeserializeOwned>(
    stream: &mut UnixStream,
    limit: usize,
) -> io::Result<T> {
    let size = stream.read_u32().await? as usize;
    if size == 0 || size > limit {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))
}

struct Buffer {
    bytes: Vec<u8>,
    limit: usize,
}

impl Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit - self.bytes.len() {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) async fn write(
    stream: &mut UnixStream,
    value: &impl Serialize,
    limit: usize,
) -> io::Result<()> {
    let mut buffer = Buffer {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut buffer, value)
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    stream.write_u32(buffer.bytes.len() as u32).await?;
    stream.write_all(&buffer.bytes).await
}
