use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CLI_CONTRACT_VERSION: u32 = 1;
pub const MAX_ASK_QUESTION_BYTES: usize = 16 * 1024;
pub const MAX_ASK_REPLY_BYTES: usize = 128 * 1024;

pub type HostFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, HostError>> + Send + 'a>>;

/// A transport authenticates the peer here, before opening a session.
/// Connector and session methods must be nonblocking and cancellation-safe when their futures drop.
pub trait Connector: Sync {
    fn default_endpoint(&self) -> Option<String> {
        None
    }

    fn connect<'a>(&'a self, endpoint: &'a str) -> HostFuture<'a, Box<dyn HostSession + 'a>>;
}

pub trait HostSession: Send + Sync {
    fn capabilities(&self) -> HostFuture<'_, Capabilities>;
    fn projects(&self) -> HostFuture<'_, Vec<OnlineProject>>;
    fn tools(&self) -> HostFuture<'_, Vec<ReadTool>>;

    /// Recheck permissions and the exact project identity/generation before engine dispatch.
    /// Mark dispatch only once the complete operation has been submitted. Receipts must expose
    /// only public results. Drop must release session resources without blocking.
    fn call<'a>(
        &'a self,
        tool: ReadTool,
        project: &'a OnlineProject,
        dispatch: &'a Dispatch,
    ) -> HostFuture<'a, ToolReply>;

    /// Check that the saved profile exists and is registered for Agent use. This is a local,
    /// read-only preflight: it must not load credentials or contact the model provider.
    fn validate_ask_profile<'a>(&'a self, _model_profile_id: &'a str) -> HostFuture<'a, ()> {
        Box::pin(async { Err(HostError::AskUnavailable) })
    }

    /// Resolve an explicitly registered Agent-purpose model profile in the host and run one
    /// text-only question with exactly the supplied, engine-scoped read allowlist. The CLI never
    /// receives model endpoints, headers, credentials, or tool definitions from the model.
    /// Implementations must recheck the saved profile purpose and enforce the allowlist again
    /// before dispatching the model or any engine tool.
    fn ask<'a>(
        &'a self,
        _model_profile_id: &'a str,
        _question: &'a str,
        _project: &'a OnlineProject,
        _allowed_tools: &'a [ReadTool],
        _dispatch: &'a Dispatch,
    ) -> HostFuture<'a, AskReply> {
        Box::pin(async { Err(HostError::AskUnavailable) })
    }
}

/// Tracks whether there is a dispatched operation whose outcome can be unknown.
#[derive(Default)]
pub struct Dispatch(AtomicBool);

impl Dispatch {
    pub fn mark_dispatched(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_dispatched(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
    pub(crate) fn confirm_not_dispatched(&self) {
        self.0.store(false, Ordering::Release);
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostError {
    EndpointUnreadable,
    EndpointOccupied,
    Authentication,
    Unavailable,
    TransportNotReady,
    Incompatible,
    ProjectNotConnected,
    ToolUnavailable,
    PermissionDenied,
    ToolFailed,
    AskUnavailable,
    ModelUnavailable,
}

/// Final text-only answer from the native host. Tool receipts and credentials stay host-local.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AskReply {
    pub text: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Capabilities {
    pub cli_contract_version: u32,
    pub project_targeting: bool,
    pub public_results: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    Unreal,
    Unity,
    Godot,
}

impl Engine {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Unreal => "unreal",
            Self::Unity => "unity",
            Self::Godot => "godot",
        }
    }
}

/// A host-owned online selection, not an entry in the on-disk project library.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OnlineProject {
    pub engine: Engine,
    pub name: String,
    pub path: PathBuf,
    /// Public selection generation. The transport must never interpret this as a credential.
    pub connection_id: String,
}

/// Closed set of supported read-only operations. No arbitrary tool name or parameters go to the host.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadTool {
    #[serde(rename = "ue_project_info")]
    UnrealProjectInfo,
    UnityProjectInfo,
    UnityActiveScene,
    GodotProjectInfo,
    GodotSceneTree,
}

impl ReadTool {
    /// Closed read-only tool set for one engine; host availability is intersected by the CLI.
    pub const fn allowlist_for(engine: Engine) -> &'static [Self] {
        const UNREAL: &[ReadTool] = &[ReadTool::UnrealProjectInfo];
        const UNITY: &[ReadTool] = &[ReadTool::UnityProjectInfo, ReadTool::UnityActiveScene];
        const GODOT: &[ReadTool] = &[ReadTool::GodotProjectInfo, ReadTool::GodotSceneTree];
        match engine {
            Engine::Unreal => UNREAL,
            Engine::Unity => UNITY,
            Engine::Godot => GODOT,
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "ue_project_info" => Some(Self::UnrealProjectInfo),
            "unity_project_info" => Some(Self::UnityProjectInfo),
            "unity_active_scene" => Some(Self::UnityActiveScene),
            "godot_project_info" => Some(Self::GodotProjectInfo),
            "godot_scene_tree" => Some(Self::GodotSceneTree),
            _ => None,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::UnrealProjectInfo => "ue_project_info",
            Self::UnityProjectInfo => "unity_project_info",
            Self::UnityActiveScene => "unity_active_scene",
            Self::GodotProjectInfo => "godot_project_info",
            Self::GodotSceneTree => "godot_scene_tree",
        }
    }

    pub const fn engine(self) -> Engine {
        match self {
            Self::UnrealProjectInfo => Engine::Unreal,
            Self::UnityProjectInfo | Self::UnityActiveScene => Engine::Unity,
            Self::GodotProjectInfo | Self::GodotSceneTree => Engine::Godot,
        }
    }

    pub(crate) const fn description(self) -> &'static str {
        match self {
            Self::UnrealProjectInfo => "Read project information from the selected Unreal editor.",
            Self::UnityProjectInfo => "Read the selected Unity editor project.",
            Self::UnityActiveScene => "Read the selected Unity active scene.",
            Self::GodotProjectInfo => "Read the selected Godot editor project.",
            Self::GodotSceneTree => "Read the selected Godot editor's current scene tree.",
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolReply {
    pub structured: Value,
    pub content: Vec<Value>,
}

/// Reports an unsupported platform without consulting other transports.
pub struct UnavailableConnector;

impl Connector for UnavailableConnector {
    fn connect<'a>(&'a self, _: &'a str) -> HostFuture<'a, Box<dyn HostSession + 'a>> {
        Box::pin(async { Err(HostError::TransportNotReady) })
    }
}
