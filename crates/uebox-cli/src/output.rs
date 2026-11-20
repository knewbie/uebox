use serde_json::{json, Value};

use crate::{HostError, OnlineProject};

#[derive(Debug, Eq, PartialEq)]
pub struct RunOutput {
    pub exit_code: u8,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Fault {
    InvalidArgument,
    ConfigMissing,
    ConfigUnreadable,
    AuthFailed,
    ServiceUnavailable,
    TransportNotReady,
    IncompatibleServer,
    ProjectNotConnected,
    ProjectAmbiguous,
    ToolUnavailable,
    RiskNotSupported,
    TimeoutBeforeDispatch,
    TimeoutAfterDispatch,
    LostAfterDispatch,
    RequestFailed,
    ToolFailed,
    AskUnavailable,
    ModelUnavailable,
}

impl Fault {
    pub(crate) fn host(error: HostError, dispatched: bool) -> Self {
        match error {
            HostError::EndpointUnreadable | HostError::EndpointOccupied => Self::ConfigUnreadable,
            HostError::Authentication => Self::AuthFailed,
            HostError::Unavailable if dispatched => Self::LostAfterDispatch,
            HostError::Unavailable => Self::ServiceUnavailable,
            HostError::TransportNotReady => Self::TransportNotReady,
            HostError::Incompatible => Self::IncompatibleServer,
            HostError::ProjectNotConnected => Self::ProjectNotConnected,
            HostError::ToolUnavailable => Self::ToolUnavailable,
            HostError::PermissionDenied => Self::RiskNotSupported,
            HostError::ToolFailed if dispatched => Self::ToolFailed,
            HostError::ToolFailed => Self::RequestFailed,
            HostError::AskUnavailable => Self::AskUnavailable,
            HostError::ModelUnavailable => Self::ModelUnavailable,
        }
    }

    fn info(self) -> (&'static str, u8, &'static str) {
        match self {
            Self::InvalidArgument => ("INVALID_ARGUMENT", 2, "Invalid command or arguments."),
            Self::ConfigMissing => (
                "CONFIG_MISSING",
                3,
                "Provide a host endpoint with --endpoint.",
            ),
            Self::ConfigUnreadable => ("CONFIG_UNREADABLE", 3, "Host endpoint access was denied."),
            Self::AuthFailed => ("AUTH_FAILED", 3, "Host peer authentication failed."),
            Self::ServiceUnavailable | Self::LostAfterDispatch => {
                ("SERVICE_UNAVAILABLE", 4, "The native host is unavailable.")
            }
            Self::TransportNotReady => (
                "SERVICE_UNAVAILABLE",
                4,
                "The native host transport is not available in this build.",
            ),
            Self::IncompatibleServer => (
                "INCOMPATIBLE_SERVER",
                4,
                "Host capability contract is incompatible.",
            ),
            Self::ProjectNotConnected => (
                "PROJECT_NOT_CONNECTED",
                5,
                "The selected project is not connected.",
            ),
            Self::ProjectAmbiguous => (
                "PROJECT_AMBIGUOUS",
                5,
                "Specify a single online project with --project.",
            ),
            Self::ToolUnavailable => ("TOOL_UNAVAILABLE", 6, "This read-only tool is unavailable."),
            Self::RiskNotSupported => (
                "RISK_NOT_SUPPORTED",
                6,
                "Only allowlisted read-only tools are supported.",
            ),
            Self::TimeoutBeforeDispatch | Self::TimeoutAfterDispatch => {
                ("TIMEOUT", 7, "The command deadline expired.")
            }
            Self::RequestFailed | Self::ToolFailed => {
                ("TOOL_FAILED", 8, "The host did not complete the read.")
            }
            Self::AskUnavailable => (
                "ASK_UNAVAILABLE",
                6,
                "This native host does not support the ask contract.",
            ),
            Self::ModelUnavailable => (
                "MODEL_UNAVAILABLE",
                6,
                "The requested registered Agent model is unavailable.",
            ),
        }
    }

    pub(crate) fn output(self, mut stderr: String) -> RunOutput {
        let (code, exit_code, message) = self.info();
        let execution = match self {
            Self::TimeoutAfterDispatch | Self::LostAfterDispatch => "unknown",
            Self::ToolFailed => "failed",
            _ => "not_started",
        };
        let document = json!({
            "schemaVersion": 1, "ok": false, "project": null,
            "error": {"code": code, "message": message, "execution": execution},
            "artifacts": [], "warnings": []
        });
        stderr.push_str(message);
        stderr.push('\n');
        RunOutput {
            exit_code,
            stdout: format!("{document}\n"),
            stderr,
        }
    }
}

pub(crate) fn success(
    data: Value,
    project: Option<&OnlineProject>,
    warnings: &[&str],
    stderr: String,
) -> RunOutput {
    let project = project.map(|p| json!({ "name": p.name, "path": p.path }));
    let document = json!({
        "schemaVersion": 1, "ok": true, "project": project, "data": data,
        "artifacts": [], "warnings": warnings
    });
    RunOutput {
        exit_code: 0,
        stdout: format!("{document}\n"),
        stderr,
    }
}
