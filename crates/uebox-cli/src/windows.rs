//! Same-user Windows named pipe IPC for the read-only CLI contract.
//!
//! The endpoint is a pipe name in the same-computer namespace (`\\.\pipe\…`), never a remote
//! `\\host\pipe\…` spelling, and never a path: unlike the Unix socket there is no file, so access
//! is decided entirely by the pipe object's descriptor. That descriptor grants exactly one trustee
//! — the user this process runs as — and both ends verify the connection before host work: the client reads
//! the pipe object's owner and DACL back with `GetSecurityInfo`, the server impersonates the
//! connected client at the identification level after the bounded hello and compares its user SID.
//! A check that fails ends
//! the connection or the listener; there is no path that continues without authentication, which is
//! the counterpart of `unix::path::authenticate` and the `0600` owner check.
//!
//! Frames, bounds, timeouts, the single in-flight request, the two-phase read and the dispatch
//! accounting are identical to `crate::unix`, so a host cannot tell the two transports apart and a
//! read cannot be started twice.

mod endpoint;
mod security;
mod wire;

#[cfg(test)]
mod tests;

use std::{future::Future, io, sync::Arc, time::Duration};

use tokio::{
    io::AsyncReadExt,
    net::windows::named_pipe::{NamedPipeClient, NamedPipeServer},
    sync::{mpsc, Mutex},
    task::JoinSet,
    time::{sleep, timeout, Instant},
};
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_BAD_NETPATH, ERROR_BAD_PIPE, ERROR_FILE_NOT_FOUND,
    ERROR_INVALID_NAME, ERROR_PATH_NOT_FOUND, ERROR_PIPE_BUSY, ERROR_PIPE_LISTENING,
};

use crate::{
    blocking, project, Capabilities, Connector, Dispatch, HostError, HostFuture, HostSession,
    OnlineProject, ReadTool, ToolReply, CLI_CONTRACT_VERSION,
};

use security::{
    authenticate_client, create_listener, open_client, verify_listener, verify_server,
    InstanceHandle, PipeSecurity, Sid,
};
use wire::{Request, Response, MAX_REQUEST, MAX_RESPONSE};

pub const PIPE_PREFIX: &str = r"\\.\pipe\";
pub const APP_IDENTIFIER: &str = "com.uebox.tauri-preview";
pub const ENDPOINT_COMPONENT: &str = "cli";

const MAX_SESSIONS: usize = 32;
/// How many instances wait for a client at any time. Beyond them a new client gets `ERROR_PIPE_BUSY`
/// and waits inside `IO_TIMEOUT`, which is the pipe equivalent of the Unix listen backlog.
const ACCEPTORS: usize = 4;
/// Instances that can be alive at once: one per running session, plus each acceptor holding the
/// connected instance and its replacement before it hands the first one on. Named pipes cap at 254.
const MAX_INSTANCES: usize = MAX_SESSIONS + 2 * ACCEPTORS;
/// Reserved outbound buffer per instance. The inbound one is exactly `MAX_REQUEST`: a request frame
/// never has to be split, and a larger reply grows on demand.
const OUT_BUFFER: u32 = 64 * 1024;
const BUSY_RETRY: Duration = Duration::from_millis(25);
const IO_TIMEOUT: Duration = Duration::from_secs(30);
const WORK_TIMEOUT: Duration = Duration::from_secs(3600);

/// The endpoint is the local pipe name carrying this user's SID. Without an identity there is no
/// endpoint: the name is never guessed and never degraded to a shared one.
pub struct WindowsConnector;

impl WindowsConnector {
    fn default_name() -> Option<String> {
        endpoint::default(&Sid::current_user().ok()?.hex())
    }
}

impl Connector for WindowsConnector {
    fn default_endpoint(&self) -> Option<String> {
        Self::default_name()
    }

    fn connect<'a>(&'a self, endpoint: &'a str) -> HostFuture<'a, Box<dyn HostSession + 'a>> {
        Box::pin(async move {
            let name = endpoint::checked(endpoint)?;
            let owner = Sid::current_user()?;
            let client = connect_client(name).await?;
            // The peer is verified before the hello frame leaves. Reading the descriptor back is
            // what proves the name belongs to this user's host, not to whoever got there first with
            // a permissive DACL.
            verify_server(&client, &owner)?;
            Ok(Box::new(Session::negotiate(client).await?) as Box<dyn HostSession>)
        })
    }
}

async fn connect_client(name: &str) -> Result<NamedPipeClient, HostError> {
    let deadline = Instant::now() + IO_TIMEOUT;
    loop {
        match open_client(name) {
            Ok(client) => return Ok(client),
            Err(error) if is(&error, ERROR_PIPE_BUSY) && Instant::now() < deadline => {
                sleep(BUSY_RETRY).await
            }
            Err(error) => return Err(connect_error(error)),
        }
    }
}

fn connect_error(error: io::Error) -> HostError {
    if is(&error, ERROR_ACCESS_DENIED) {
        // The descriptor refuses us, so the pipe is somebody else's. That is an authentication
        // failure and the connection ends here — it is never retried without the descriptor.
        HostError::Authentication
    } else if is(&error, ERROR_FILE_NOT_FOUND)
        || is(&error, ERROR_PATH_NOT_FOUND)
        || is(&error, ERROR_BAD_NETPATH)
    {
        // No instance of that name exists: the host is not running, which is an outage, not a
        // configuration problem.
        HostError::Unavailable
    } else if is(&error, ERROR_INVALID_NAME)
        || matches!(
            error.kind(),
            io::ErrorKind::InvalidInput | io::ErrorKind::PermissionDenied
        )
    {
        HostError::EndpointUnreadable
    } else {
        HostError::Unavailable
    }
}

fn listener_error(error: io::Error) -> HostError {
    if is(&error, ERROR_ACCESS_DENIED)
        || is(&error, ERROR_PIPE_BUSY)
        || is(&error, ERROR_BAD_PIPE)
        || is(&error, ERROR_PIPE_LISTENING)
    {
        // Somebody else owns the name, or its instance count no longer matches ours. The first
        // instance flag is what turns "the name is taken" into an error instead of a shared pipe,
        // and no fallback creates a later instance to take over.
        HostError::EndpointOccupied
    } else if is(&error, ERROR_INVALID_NAME)
        || is(&error, ERROR_PATH_NOT_FOUND)
        || matches!(
            error.kind(),
            io::ErrorKind::InvalidInput | io::ErrorKind::PermissionDenied
        )
    {
        HostError::EndpointUnreadable
    } else {
        HostError::Unavailable
    }
}

fn is(error: &io::Error, code: u32) -> bool {
    error.raw_os_error() == Some(code as i32)
}

struct Session {
    capabilities: Capabilities,
    // Moving the pipe out of its slot means cancellation closes the session instead of letting the
    // next invocation consume a partial frame or a stale reply.
    stream: Mutex<Option<NamedPipeClient>>,
}

impl Session {
    async fn negotiate(mut pipe: NamedPipeClient) -> Result<Self, HostError> {
        let response = exchange(
            &mut pipe,
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
            stream: Mutex::new(Some(pipe)),
        })
    }

    async fn request(&self, request: Request) -> Result<Response, HostError> {
        let mut slot = self.stream.lock().await;
        let mut pipe = slot.take().ok_or(HostError::Unavailable)?;
        let result = exchange(&mut pipe, request).await?;
        *slot = Some(pipe);
        Ok(result)
    }
}

async fn exchange(pipe: &mut NamedPipeClient, request: Request) -> Result<Response, HostError> {
    wire::write(pipe, &request, MAX_REQUEST)
        .await
        .map_err(wire_error)?;
    wire::read(pipe, MAX_RESPONSE).await.map_err(wire_error)
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
            let mut pipe = slot.take().ok_or(HostError::Unavailable)?;
            match exchange(
                &mut pipe,
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
            wire::write(&mut pipe, &Request::ExecuteRead, MAX_REQUEST)
                .await
                .map_err(wire_error)?;
            // A partial execute frame cannot start any work. Once the complete frame is written,
            // the caller can no longer infer the outcome from an absent or malformed receipt.
            dispatch.mark_dispatched();
            let response = wire::read(&mut pipe, MAX_RESPONSE)
                .await
                .map_err(|_| HostError::Unavailable)?;
            match response {
                Response::Read { receipt } => {
                    *slot = Some(pipe);
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
}

/// Bind inside a Tokio runtime with I/O enabled.
///
/// The name is established by the first instance, with the descriptor already applied: an existing
/// name is refused instead of being inherited or overwritten, exactly as the Unix side refuses an
/// existing socket file and never unlinks it. There is nothing to remove afterwards — the name
/// disappears with the last instance handle, and only this server's own instances are ever closed.
pub struct WindowsServer {
    endpoint: String,
    owner: Arc<Sid>,
    security: Arc<PipeSecurity>,
    first: NamedPipeServer,
}

struct Shared {
    endpoint: String,
    owner: Arc<Sid>,
    security: Arc<PipeSecurity>,
    host: Arc<dyn HostSession>,
}

impl WindowsServer {
    pub fn bind(endpoint: &str) -> Result<Self, HostError> {
        let endpoint = endpoint::checked(endpoint)?.to_owned();
        let owner = Arc::new(Sid::current_user()?);
        let security = Arc::new(PipeSecurity::private(&owner)?);
        let first = create_listener(&endpoint, true, &security).map_err(listener_error)?;
        // The descriptor is read back from the object, not assumed from the request.
        verify_listener(&first, &owner)?;
        Ok(Self {
            endpoint,
            owner,
            security,
            first,
        })
    }

    /// Run as part of the host runtime. Shutdown closes active sessions. Host callbacks must be
    /// safe to drop and support concurrent sessions.
    pub async fn serve(
        self,
        host: Arc<dyn HostSession>,
        shutdown: impl Future<Output = ()> + Send,
    ) -> Result<(), HostError> {
        let state = Arc::new(Shared {
            endpoint: self.endpoint,
            owner: self.owner,
            security: self.security,
            host,
        });
        let (send, mut connected) = mpsc::channel::<NamedPipeServer>(ACCEPTORS);
        let mut acceptors = JoinSet::new();
        acceptors.spawn(accept(state.clone(), send.clone(), Some(self.first)));
        while acceptors.len() < ACCEPTORS {
            acceptors.spawn(accept(state.clone(), send.clone(), None));
        }
        // The supervisor owns the only other sender; dropping it is how "no acceptor is left"
        // becomes visible to `connected.recv()`.
        drop(send);
        let mut sessions = JoinSet::new();
        tokio::pin!(shutdown);
        let result = loop {
            tokio::select! {
                biased;
                _ = &mut shutdown => break Ok(()),
                // An acceptor that cannot create or re-verify its instance has lost the name. That
                // is a serving failure, reported instead of being worked around unauthenticated.
                done = acceptors.join_next(), if !acceptors.is_empty() => match done {
                    Some(Ok(Err(error))) => break Err(error),
                    Some(Err(_)) => break Err(HostError::Unavailable),
                    _ => {}
                },
                _ = sessions.join_next(), if !sessions.is_empty() => {}
                pipe = connected.recv() => {
                    let Some(pipe) = pipe else { break Ok(()) };
                    if sessions.len() < MAX_SESSIONS {
                        let state = Arc::clone(&state);
                        sessions.spawn(async move {
                            let _ = serve_session(pipe, &state).await;
                        });
                    }
                    // Over the cap the instance is dropped here: the client reads EOF and reports
                    // the host unavailable, as on Unix.
                }
            }
        };
        sessions.shutdown().await;
        acceptors.shutdown().await;
        result
    }
}

async fn accept(
    state: Arc<Shared>,
    send: mpsc::Sender<NamedPipeServer>,
    first: Option<NamedPipeServer>,
) -> Result<(), HostError> {
    let mut pipe = match first {
        Some(pipe) => pipe,
        None => new_instance(&state)?,
    };
    loop {
        match pipe.connect().await {
            Ok(()) => {}
            Err(error) => return Err(listener_error(error)),
        }
        // The replacement is created before this instance leaves the pool, so there is never a
        // moment with no listener waiting.
        let next = new_instance(&state)?;
        if send.send(pipe).await.is_err() {
            return Ok(());
        }
        pipe = next;
    }
}

fn new_instance(state: &Shared) -> Result<NamedPipeServer, HostError> {
    let pipe = create_listener(&state.endpoint, false, &state.security).map_err(listener_error)?;
    // A later instance inherits the first one's descriptor, so reading it back is what proves the
    // name is still ours rather than somebody else's with the same spelling.
    verify_listener(&pipe, &state.owner)?;
    Ok(pipe)
}

async fn verify_client(state: &Shared, pipe: &NamedPipeServer) -> Result<(), HostError> {
    // The impersonation runs on a thread created for it alone, so the client's token cannot outlive
    // the call even if this task is cancelled or something unwinds on the runtime thread.
    let handle = InstanceHandle::of(pipe)?;
    let owner = Arc::clone(&state.owner);
    blocking(
        move || authenticate_client(handle, owner.as_ref()),
        HostError::Authentication,
    )
    .await
}

async fn read_request(pipe: &mut NamedPipeServer) -> io::Result<Request> {
    timeout(IO_TIMEOUT, wire::read(pipe, MAX_REQUEST)).await?
}

async fn reply(pipe: &mut NamedPipeServer, reply: Response) -> io::Result<()> {
    timeout(IO_TIMEOUT, wire::write(pipe, &reply, MAX_RESPONSE)).await?
}

async fn while_connected<T>(
    pipe: &mut NamedPipeServer,
    work: impl Future<Output = T>,
) -> io::Result<T> {
    let mut unexpected = [0];
    tokio::select! {
        biased;
        // The protocol allows one in-flight request. EOF or pipelining closes the session and
        // drops the host work. A read already sent to the editor may still complete there.
        _ = pipe.read(&mut unexpected) => Err(io::Error::from(io::ErrorKind::ConnectionAborted)),
        result = timeout(WORK_TIMEOUT, work) => Ok(result?),
    }
}

async fn serve_session(mut pipe: NamedPipeServer, state: &Shared) -> io::Result<()> {
    // ImpersonateNamedPipeClient uses the identity of the last message read. The pipe's
    // own DACL already guards this bounded hello. Verify the token before replying or dispatching.
    let hello = read_request(&mut pipe).await?;
    verify_client(state, &pipe)
        .await
        .map_err(|_| io::Error::from(io::ErrorKind::PermissionDenied))?;
    if !matches!(
        hello,
        Request::Hello {
            version: CLI_CONTRACT_VERSION
        }
    ) {
        return reply(&mut pipe, Response::rejected(HostError::Incompatible)).await;
    }
    let host = state.host.as_ref();
    let capabilities = match while_connected(&mut pipe, host.capabilities()).await? {
        Ok(value) => value,
        Err(error) => return reply(&mut pipe, Response::rejected(error)).await,
    };
    reply(&mut pipe, Response::Hello { capabilities }).await?;
    if capabilities.cli_contract_version != CLI_CONTRACT_VERSION
        || !capabilities.project_targeting
        || !capabilities.public_results
    {
        return Ok(());
    }

    loop {
        let response = match read_request(&mut pipe).await? {
            Request::Projects => match while_connected(&mut pipe, online(host)).await? {
                Ok(projects) => Response::Projects { projects },
                Err(error) => Response::rejected(error),
            },
            Request::Tools => match while_connected(&mut pipe, host.tools()).await? {
                Ok(tools) => Response::Tools { tools },
                Err(error) => Response::rejected(error),
            },
            Request::PrepareRead {
                tool,
                connection_id,
            } => read_tool(&mut pipe, host, tool, connection_id).await?,
            Request::Hello { .. } | Request::ExecuteRead => {
                return reply(&mut pipe, Response::rejected(HostError::Incompatible)).await;
            }
        };
        reply(&mut pipe, response).await?;
    }
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
    pipe: &mut NamedPipeServer,
    host: &dyn HostSession,
    tool: ReadTool,
    connection_id: String,
) -> io::Result<Response> {
    let project = match while_connected(pipe, prepare(host, tool, &connection_id)).await? {
        Ok(project) => project,
        Err(error) => return Ok(Response::rejected(error)),
    };
    reply(pipe, Response::Ready).await?;
    if !matches!(read_request(pipe).await?, Request::ExecuteRead) {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    // The host owns the generation, and must revalidate it atomically at admission.
    let dispatch = Dispatch::default();
    let result = while_connected(pipe, host.call(tool, &project, &dispatch)).await?;
    Ok(match result {
        Ok(receipt) => Response::Read { receipt },
        Err(error) => Response::Failure {
            error,
            dispatched: dispatch.is_dispatched(),
        },
    })
}
