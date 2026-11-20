//! Same-user Unix IPC for the read-only CLI contract (macOS/Linux).

mod path;
#[cfg(test)]
mod tests;
mod wire;

use std::{
    future::Future,
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use tokio::{
    io::AsyncReadExt,
    net::{UnixListener, UnixStream},
    sync::Mutex,
    task::JoinSet,
    time::timeout,
};

use crate::{
    blocking, project, AskReply, Capabilities, Connector, Dispatch, HostError, HostFuture,
    HostSession, OnlineProject, ReadTool, ToolReply, CLI_CONTRACT_VERSION, MAX_ASK_QUESTION_BYTES,
    MAX_ASK_REPLY_BYTES,
};

use path::{authenticate, io_error, Endpoint, SocketGuard};
use wire::{Request, Response, MAX_REQUEST, MAX_RESPONSE};

pub const SOCKET_NAME: &str = "uebox.sock";
pub const APP_IDENTIFIER: &str = "com.uebox.tauri-preview";
pub const ENDPOINT_DIRECTORY: &str = "cli";
const MAX_SESSIONS: usize = 32;
const IO_TIMEOUT: Duration = Duration::from_secs(30);
const WORK_TIMEOUT: Duration = Duration::from_secs(3600);

/// The endpoint is the absolute, existing, same-user 0700 runtime directory.
/// The connector verifies the filesystem identity and kernel peer UID before sending a frame.
pub struct UnixConnector;

impl Connector for UnixConnector {
    fn default_endpoint(&self) -> Option<String> {
        dirs::data_local_dir()
            .filter(|directory| directory.is_absolute())?
            .join(APP_IDENTIFIER)
            .join(ENDPOINT_DIRECTORY)
            .into_os_string()
            .into_string()
            .ok()
    }

    fn connect<'a>(&'a self, endpoint: &'a str) -> HostFuture<'a, Box<dyn HostSession + 'a>> {
        Box::pin(async move {
            let directory = PathBuf::from(endpoint);
            let (endpoint, identity) = blocking(
                move || {
                    let endpoint = Endpoint::checked(&directory)?;
                    let identity = endpoint.socket_identity()?;
                    Ok((endpoint, identity))
                },
                HostError::EndpointUnreadable,
            )
            .await?;
            let stream = UnixStream::connect(&endpoint.socket)
                .await
                .map_err(io_error)?;
            authenticate(&stream, endpoint.uid)?;
            blocking(
                move || {
                    if endpoint.socket_identity()? == identity {
                        Ok(())
                    } else {
                        Err(HostError::EndpointUnreadable)
                    }
                },
                HostError::EndpointUnreadable,
            )
            .await?;
            Ok(Box::new(Session::negotiate(stream).await?) as Box<dyn HostSession>)
        })
    }
}

struct Session {
    capabilities: Capabilities,
    // Moving the socket out of its slot means cancellation closes the session instead of letting
    // the next invocation consume a partial frame or a stale reply.
    stream: Mutex<Option<UnixStream>>,
}

impl Session {
    async fn negotiate(mut stream: UnixStream) -> Result<Self, HostError> {
        let response = exchange(
            &mut stream,
            Request::Hello {
                version: CLI_CONTRACT_VERSION,
            },
        )
        .await?;
        let capabilities = match response {
            Response::Hello { capabilities } => capabilities,
            Response::Failure { error, .. } => return Err(error),
            _ => return Err(HostError::Incompatible),
        };
        Ok(Self {
            capabilities,
            stream: Mutex::new(Some(stream)),
        })
    }

    async fn request(&self, request: Request) -> Result<Response, HostError> {
        let mut slot = self.stream.lock().await;
        let mut stream = slot.take().ok_or(HostError::Unavailable)?;
        let result = exchange(&mut stream, request).await?;
        *slot = Some(stream);
        Ok(result)
    }
}

async fn exchange(stream: &mut UnixStream, request: Request) -> Result<Response, HostError> {
    wire::write(stream, &request, MAX_REQUEST)
        .await
        .map_err(wire_error)?;
    wire::read(stream, MAX_RESPONSE).await.map_err(wire_error)
}

fn wire_error(error: io::Error) -> HostError {
    if error.kind() == io::ErrorKind::InvalidData {
        HostError::Incompatible
    } else {
        HostError::Unavailable
    }
}

impl HostSession for Session {
    fn capabilities(&self) -> HostFuture<'_, Capabilities> {
        Box::pin(async { Ok(self.capabilities) })
    }

    fn projects(&self) -> HostFuture<'_, Vec<OnlineProject>> {
        Box::pin(async {
            match self.request(Request::Projects).await? {
                Response::Projects { projects } => Ok(projects),
                Response::Failure { error, .. } => Err(error),
                _ => Err(HostError::Incompatible),
            }
        })
    }

    fn tools(&self) -> HostFuture<'_, Vec<ReadTool>> {
        Box::pin(async {
            match self.request(Request::Tools).await? {
                Response::Tools { tools } => Ok(tools),
                Response::Failure { error, .. } => Err(error),
                _ => Err(HostError::Incompatible),
            }
        })
    }

    fn call<'a>(
        &'a self,
        tool: ReadTool,
        project: &'a OnlineProject,
        dispatch: &'a Dispatch,
    ) -> HostFuture<'a, ToolReply> {
        Box::pin(async move {
            let mut slot = self.stream.lock().await;
            let mut stream = slot.take().ok_or(HostError::Unavailable)?;
            match exchange(
                &mut stream,
                Request::PrepareRead {
                    tool,
                    connection_id: project.connection_id.clone(),
                },
            )
            .await?
            {
                Response::Ready => {}
                Response::Failure { error, .. } => return Err(error),
                _ => return Err(HostError::Incompatible),
            }
            wire::write(&mut stream, &Request::ExecuteRead, MAX_REQUEST)
                .await
                .map_err(wire_error)?;
            // A partial execute frame cannot start any work. Once the complete frame is written,
            // the caller can no longer infer the outcome from an absent or malformed receipt.
            dispatch.mark_dispatched();
            let response = wire::read(&mut stream, MAX_RESPONSE)
                .await
                .map_err(|_| HostError::Unavailable)?;
            match response {
                Response::Read { receipt } => {
                    *slot = Some(stream);
                    Ok(receipt)
                }
                Response::Failure { error, dispatched } => {
                    if !dispatched {
                        dispatch.confirm_not_dispatched();
                    }
                    Err(error)
                }
                _ => Err(HostError::Unavailable),
            }
        })
    }

    fn ask<'a>(
        &'a self,
        model_profile_id: &'a str,
        question: &'a str,
        project: &'a OnlineProject,
        allowed_tools: &'a [ReadTool],
        dispatch: &'a Dispatch,
    ) -> HostFuture<'a, AskReply> {
        Box::pin(async move {
            if !valid_profile_id(model_profile_id)
                || question.trim().is_empty()
                || question.len() > MAX_ASK_QUESTION_BYTES
                || project.connection_id.is_empty()
                || project.connection_id.len() > 256
                || project.connection_id.chars().any(char::is_control)
                || allowed_tools.is_empty()
                || allowed_tools
                    .iter()
                    .any(|tool| !ReadTool::allowlist_for(project.engine).contains(tool))
            {
                return Err(HostError::Incompatible);
            }

            let mut slot = self.stream.lock().await;
            let mut stream = slot.take().ok_or(HostError::Unavailable)?;
            match exchange(
                &mut stream,
                Request::PrepareAsk {
                    project_id: project.connection_id.clone(),
                    model_profile_id: model_profile_id.to_owned(),
                    question: question.to_owned(),
                },
            )
            .await?
            {
                Response::Ready => {}
                Response::Failure { error, .. } => return Err(error),
                _ => return Err(HostError::Incompatible),
            }

            wire::write(&mut stream, &Request::ExecuteAsk, MAX_REQUEST)
                .await
                .map_err(wire_error)?;
            dispatch.mark_dispatched();
            let response = wire::read(&mut stream, MAX_RESPONSE)
                .await
                .map_err(|_| HostError::Unavailable)?;
            match response {
                Response::Ask { reply } if reply.text.len() <= MAX_ASK_REPLY_BYTES => {
                    *slot = Some(stream);
                    Ok(reply)
                }
                Response::Ask { .. } => Err(HostError::ToolFailed),
                Response::Failure { error, dispatched } => {
                    if !dispatched {
                        dispatch.confirm_not_dispatched();
                    }
                    Err(error)
                }
                _ => Err(HostError::Unavailable),
            }
        })
    }
}

/// Bind after the host has provisioned its private directory, within a Tokio runtime with I/O enabled.
/// An existing socket is an error, even if its previous owner process has exited.
pub struct UnixServer {
    listener: UnixListener,
    socket: SocketGuard,
}

impl UnixServer {
    pub fn bind(directory: &Path) -> Result<Self, HostError> {
        let (listener, socket) = Endpoint::checked(directory)?.bind()?;
        Ok(Self { listener, socket })
    }

    /// Run as part of the host runtime. Shutdown closes active sessions and removes only this
    /// server's own socket. Host callbacks must be safe to drop and support concurrent sessions.
    pub async fn serve(
        self,
        host: Arc<dyn HostSession>,
        shutdown: impl Future<Output = ()> + Send,
    ) -> Result<(), HostError> {
        let mut sessions = JoinSet::new();
        tokio::pin!(shutdown);
        let result = loop {
            tokio::select! {
                _ = &mut shutdown => break Ok(()),
                _ = sessions.join_next(), if !sessions.is_empty() => {},
                incoming = self.listener.accept() => {
                    let (stream, _) = match incoming {
                        Ok(incoming) => incoming,
                        Err(error) => break Err(io_error(error)),
                    };
                    if sessions.len() < MAX_SESSIONS && authenticate(&stream, self.socket.endpoint.uid).is_ok() {
                        let host = Arc::clone(&host);
                        sessions.spawn(async move { let _ = serve_session(stream, host.as_ref()).await; });
                    }
                }
            }
        };
        sessions.shutdown().await;
        result
    }
}

async fn read_request(stream: &mut UnixStream) -> io::Result<Request> {
    timeout(IO_TIMEOUT, wire::read(stream, MAX_REQUEST)).await?
}

async fn reply(stream: &mut UnixStream, reply: Response) -> io::Result<()> {
    timeout(IO_TIMEOUT, wire::write(stream, &reply, MAX_RESPONSE)).await?
}

async fn while_connected<T>(
    stream: &mut UnixStream,
    work: impl Future<Output = T>,
) -> io::Result<T> {
    let mut unexpected = [0];
    tokio::select! {
        biased;
        // The protocol allows one in-flight request. EOF or pipelining closes the session and
        // drops the host work. A read already sent to the editor may still complete there.
        _ = stream.read(&mut unexpected) => Err(io::Error::from(io::ErrorKind::ConnectionAborted)),
        result = timeout(WORK_TIMEOUT, work) => Ok(result?),
    }
}

async fn serve_session(mut stream: UnixStream, host: &dyn HostSession) -> io::Result<()> {
    if !matches!(
        read_request(&mut stream).await?,
        Request::Hello {
            version: CLI_CONTRACT_VERSION
        }
    ) {
        return reply(&mut stream, Response::rejected(HostError::Incompatible)).await;
    }
    let capabilities = match while_connected(&mut stream, host.capabilities()).await? {
        Ok(value) => value,
        Err(error) => return reply(&mut stream, Response::rejected(error)).await,
    };
    reply(&mut stream, Response::Hello { capabilities }).await?;
    if capabilities.cli_contract_version != CLI_CONTRACT_VERSION
        || !capabilities.project_targeting
        || !capabilities.public_results
    {
        return Ok(());
    }

    loop {
        let response = match read_request(&mut stream).await? {
            Request::Projects => match while_connected(&mut stream, online(host)).await? {
                Ok(projects) => Response::Projects { projects },
                Err(error) => Response::rejected(error),
            },
            Request::Tools => match while_connected(&mut stream, host.tools()).await? {
                Ok(tools) => Response::Tools { tools },
                Err(error) => Response::rejected(error),
            },
            Request::PrepareRead {
                tool,
                connection_id,
            } => read_tool(&mut stream, host, tool, connection_id).await?,
            Request::PrepareAsk {
                project_id,
                model_profile_id,
                question,
            } => ask_tool(&mut stream, host, project_id, model_profile_id, question).await?,
            Request::Hello { .. } | Request::ExecuteRead | Request::ExecuteAsk => {
                return reply(&mut stream, Response::rejected(HostError::Incompatible)).await;
            }
        };
        reply(&mut stream, response).await?;
    }
}

struct PreparedAsk {
    project: OnlineProject,
    model_profile_id: String,
    question: String,
    allowed_tools: Vec<ReadTool>,
}

async fn prepare_ask(
    host: &dyn HostSession,
    project_id: &str,
    model_profile_id: &str,
    question: String,
) -> Result<PreparedAsk, HostError> {
    if project_id.is_empty()
        || project_id.len() > 256
        || project_id.chars().any(char::is_control)
        || !valid_profile_id(model_profile_id)
        || question.trim().is_empty()
        || question.len() > MAX_ASK_QUESTION_BYTES
    {
        return Err(HostError::Incompatible);
    }

    let project = online(host)
        .await?
        .into_iter()
        .find(|project| project.connection_id == project_id)
        .ok_or(HostError::ProjectNotConnected)?;
    host.validate_ask_profile(model_profile_id).await?;
    let exposed = host.tools().await?;
    let allowed_tools: Vec<_> = ReadTool::allowlist_for(project.engine)
        .iter()
        .copied()
        .filter(|tool| exposed.contains(tool))
        .collect();
    if allowed_tools.is_empty() {
        return Err(HostError::ToolUnavailable);
    }

    Ok(PreparedAsk {
        project,
        model_profile_id: model_profile_id.to_owned(),
        question,
        allowed_tools,
    })
}

async fn ask_tool(
    stream: &mut UnixStream,
    host: &dyn HostSession,
    project_id: String,
    model_profile_id: String,
    question: String,
) -> io::Result<Response> {
    let prepared = match while_connected(
        stream,
        prepare_ask(host, &project_id, &model_profile_id, question),
    )
    .await?
    {
        Ok(prepared) => prepared,
        Err(error) => return Ok(Response::rejected(error)),
    };

    reply(stream, Response::Ready).await?;
    if !matches!(read_request(stream).await?, Request::ExecuteAsk) {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }

    let dispatch = Dispatch::default();
    let result = while_connected(
        stream,
        host.ask(
            &prepared.model_profile_id,
            &prepared.question,
            &prepared.project,
            &prepared.allowed_tools,
            &dispatch,
        ),
    )
    .await?;
    Ok(match result {
        Ok(reply) if reply.text.len() <= MAX_ASK_REPLY_BYTES => Response::Ask { reply },
        Ok(_) => Response::Failure {
            error: HostError::ToolFailed,
            dispatched: dispatch.is_dispatched(),
        },
        Err(error) => Response::Failure {
            error,
            dispatched: dispatch.is_dispatched(),
        },
    })
}

fn valid_profile_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

async fn online(host: &dyn HostSession) -> Result<Vec<OnlineProject>, HostError> {
    let projects = host.projects().await?;
    project::validate(&projects).map_err(|_| HostError::Incompatible)?;
    Ok(projects)
}

async fn prepare(
    host: &dyn HostSession,
    tool: ReadTool,
    connection_id: &str,
) -> Result<OnlineProject, HostError> {
    if connection_id.is_empty()
        || connection_id.len() > 256
        || connection_id.chars().any(char::is_control)
    {
        return Err(HostError::ProjectNotConnected);
    }
    if !host.tools().await?.contains(&tool) {
        return Err(HostError::ToolUnavailable);
    }
    online(host)
        .await?
        .into_iter()
        .find(|project| project.connection_id == connection_id && project.engine == tool.engine())
        .ok_or(HostError::ProjectNotConnected)
}

async fn read_tool(
    stream: &mut UnixStream,
    host: &dyn HostSession,
    tool: ReadTool,
    connection_id: String,
) -> io::Result<Response> {
    let project = match while_connected(stream, prepare(host, tool, &connection_id)).await? {
        Ok(project) => project,
        Err(error) => return Ok(Response::rejected(error)),
    };
    reply(stream, Response::Ready).await?;
    if !matches!(read_request(stream).await?, Request::ExecuteRead) {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    // The host owns the generation, and must revalidate it atomically at admission.
    let dispatch = Dispatch::default();
    let result = while_connected(stream, host.call(tool, &project, &dispatch)).await?;
    Ok(match result {
        Ok(receipt) => Response::Read { receipt },
        Err(error) => Response::Failure {
            error,
            dispatched: dispatch.is_dispatched(),
        },
    })
}
