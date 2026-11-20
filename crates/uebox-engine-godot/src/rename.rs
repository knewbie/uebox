use super::{valid_hex, Error, GodotClient};
use serde::Deserialize;
use serde_json::json;
use std::time::{Duration, Instant};

const PREPARE: &str = "scene.prepare_rename";
const APPLY: &str = "scene.apply_rename";

/// The editor ticket is private and consumed by apply.
pub struct PreparedNodeRename {
    receipt: Prepared,
    instance_id: String,
    expires_at: Instant,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Prepared {
    node_path: String,
    old_name: String,
    new_name: String,
    scene_path: Option<String>,
    scene_id: String,
    node_id: String,
    ticket: String,
    expires_in_ms: u64,
}

impl PreparedNodeRename {
    pub fn node_path(&self) -> &str {
        &self.receipt.node_path
    }

    pub fn old_name(&self) -> &str {
        &self.receipt.old_name
    }

    pub fn new_name(&self) -> &str {
        &self.receipt.new_name
    }

    pub fn scene_path(&self) -> Option<&str> {
        self.receipt.scene_path.as_deref()
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum NodeRenameOutcome {
    Applied { actual_name: String },
    Mismatch { actual_name: String },
    NotExecuted { reason: &'static str },
    Unknown,
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum ResultData {
    Applied {
        actual_name: String,
        scene_id: String,
        node_id: String,
    },
    Mismatch {
        actual_name: String,
        scene_id: String,
        node_id: String,
    },
    NotExecuted {
        reason: String,
    },
    Unknown,
}

impl GodotClient {
    pub fn prepare_node_rename(&self, path: &str, name: &str) -> Result<PreparedNodeRename, Error> {
        if !valid_path(path) || !valid_name(name) {
            return Err(Error::Remote {
                code: "invalid_request".into(),
                message: "invalid node path or name".into(),
            });
        }
        let started = Instant::now();
        let receipt: Prepared = self.query_with(
            PREPARE,
            Some(json!({"node_path":path,"new_name":name})),
            &mut false,
        )?;
        if receipt.node_path != path
            || receipt.new_name != name
            || !bounded_text(&receipt.old_name, 4096)
            || receipt
                .scene_path
                .as_deref()
                .is_some_and(|value| !bounded_text(value, 4096))
            || !object_id(&receipt.scene_id)
            || !object_id(&receipt.node_id)
            || !(valid_hex(&receipt.ticket, 32) || valid_hex(&receipt.ticket, 64))
            || !(1..=60_000).contains(&receipt.expires_in_ms)
            || self.contains_private_bridge_values(&receipt.old_name)
            || self.contains_private_bridge_values(&receipt.new_name)
            || self.contains_private_bridge_values(&receipt.node_path)
            || receipt
                .scene_path
                .as_deref()
                .is_some_and(|value| self.contains_private_bridge_values(value))
        {
            return Err(Error::Protocol("invalid rename preview".into()));
        }
        let expires_at = started + Duration::from_millis(receipt.expires_in_ms);
        Ok(PreparedNodeRename {
            receipt,
            instance_id: self.target.instance_id.clone(),
            expires_at,
        })
    }

    pub fn apply_node_rename(&self, prepared: PreparedNodeRename) -> NodeRenameOutcome {
        if prepared.instance_id != self.target.instance_id {
            return rejected("target_changed");
        }
        if Instant::now() >= prepared.expires_at {
            return rejected("ticket_expired");
        }
        let mut dispatched = false;
        let result = self.query_with(
            APPLY,
            Some(json!({"ticket":prepared.receipt.ticket})),
            &mut dispatched,
        );
        match result {
            Ok(result) => self.project_rename_result(result, prepared.receipt),
            Err(_) if !dispatched => rejected("bridge_unavailable"),
            Err(_) => NodeRenameOutcome::Unknown,
        }
    }

    fn project_rename_result(&self, result: ResultData, expected: Prepared) -> NodeRenameOutcome {
        let check = |name: &str, scene: &str, node: &str| {
            scene == expected.scene_id
                && node == expected.node_id
                && bounded_text(name, 4096)
                && !self.contains_private_bridge_values(name)
        };
        match result {
            ResultData::Applied {
                actual_name,
                scene_id,
                node_id,
            } if actual_name == expected.new_name && check(&actual_name, &scene_id, &node_id) => {
                NodeRenameOutcome::Applied { actual_name }
            }
            ResultData::Mismatch {
                actual_name,
                scene_id,
                node_id,
            } if check(&actual_name, &scene_id, &node_id) => {
                NodeRenameOutcome::Mismatch { actual_name }
            }
            ResultData::NotExecuted { reason } => match reason.as_str() {
                "ticket_invalid" | "invalid_request" => rejected("ticket_invalid"),
                "ticket_expired" | "expired_or_consumed" => rejected("ticket_expired"),
                "target_changed" | "context_changed" | "node_changed" | "name_changed" => {
                    rejected("target_changed")
                }
                "name_taken" | "name_collision" => rejected("name_taken"),
                "undo_unavailable" => rejected("undo_unavailable"),
                _ => NodeRenameOutcome::Unknown,
            },
            _ => NodeRenameOutcome::Unknown,
        }
    }
}

fn object_id(value: &str) -> bool {
    value
        .parse::<u64>()
        .ok()
        .is_some_and(|id| id > 0 && id.to_string() == value)
}

fn bounded_text(value: &str, max: usize) -> bool {
    value.len() <= max && !value.chars().any(char::is_control)
}

fn valid_path(path: &str) -> bool {
    bounded_text(path, 4096)
        && !path.contains([':', '\\', '%'])
        && path
            .split('/')
            .all(|segment| !matches!(segment, "" | "." | ".."))
}

fn valid_name(name: &str) -> bool {
    !name.trim().is_empty()
        && bounded_text(name, 256)
        && !name.contains(['.', ':', '@', '/', '"', '%', '\\'])
}

fn rejected(reason: &'static str) -> NodeRenameOutcome {
    NodeRenameOutcome::NotExecuted { reason }
}
