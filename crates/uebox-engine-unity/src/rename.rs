use super::{invalid_field, query_data, query_project, BridgeError, UnityClient};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

const COMMAND: &str = "uebox_prepare_or_apply_gameobject_name";

/// An opaque, one-use editor ticket bound to the connection that prepared it.
pub struct PreparedGameObjectName {
    connection_id: u64,
    scene_handle: i32,
    game_object_instance_id: i32,
    old_name: String,
    proposed_name: String,
    ticket: String,
    expires: Instant,
}

impl PreparedGameObjectName {
    pub fn scene_handle(&self) -> i32 {
        self.scene_handle
    }

    pub fn game_object_instance_id(&self) -> i32 {
        self.game_object_instance_id
    }

    pub fn old_name(&self) -> &str {
        &self.old_name
    }

    pub fn proposed_name(&self) -> &str {
        &self.proposed_name
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum GameObjectNameOutcome {
    Applied { actual_name: String },
    Mismatch { actual_name: String },
    NotExecuted { reason: &'static str },
    Unknown,
}

impl UnityClient {
    pub fn prepare_selected_game_object_name(
        &mut self,
        proposed_name: &str,
    ) -> Result<PreparedGameObjectName, BridgeError> {
        if !valid_name(proposed_name) {
            return Err(invalid_field("newName"));
        }
        let mut stream = self.stream.take().ok_or(BridgeError::Disconnected)?;
        let identity = query_project(
            &mut stream,
            &self.target,
            &self.canonical_project_root,
            self.options,
        )?;
        let started = Instant::now();
        let reply = query_data(
            &mut stream,
            json!({"type": COMMAND,"params":{"action":"prepare","newName":proposed_name}}),
            self.options,
        );
        // An absent optional tool or a declined selection cannot mutate the editor,
        // and a complete framed error keeps this read-only connection usable.
        if reply.is_ok() || matches!(reply, Err(BridgeError::Remote(_))) {
            self.identity = identity;
            self.stream = Some(stream);
        }
        let data = reply?;
        let preview = &data["preview"];
        let scene_handle = wire_integer(&preview["sceneHandle"], "sceneHandle")?;
        let game_object_instance_id =
            wire_integer(&preview["gameObjectInstanceId"], "gameObjectInstanceId")?;
        let old_name =
            read_back_name(&preview["oldName"]).ok_or_else(|| invalid_field("oldName"))?;
        if data["proposedName"].as_str() != Some(proposed_name) {
            return Err(invalid_field("proposedName"));
        }
        let ticket = data["ticket"]
            .as_str()
            .filter(|ticket| ticket.len() == 64 && ticket.bytes().all(|c| c.is_ascii_hexdigit()))
            .ok_or_else(|| invalid_field("ticket"))?;
        let ttl = data["expiresInSeconds"]
            .as_f64()
            .filter(|seconds| (1.0..=30.0).contains(seconds))
            .ok_or_else(|| invalid_field("expiresInSeconds"))?;

        Ok(PreparedGameObjectName {
            connection_id: self.connection_id,
            scene_handle,
            game_object_instance_id,
            old_name: old_name.to_owned(),
            proposed_name: proposed_name.to_owned(),
            ticket: ticket.to_owned(),
            expires: started + Duration::from_secs_f64(ttl),
        })
    }

    /// Apply exactly one proposal. A lost response after dispatch is unknown and
    /// the connection closes so the application can require explicit recovery.
    pub fn apply_selected_game_object_name(
        &mut self,
        prepared: PreparedGameObjectName,
    ) -> GameObjectNameOutcome {
        if self.connection_id != prepared.connection_id {
            return rejected("target_changed");
        }
        if Instant::now() >= prepared.expires {
            return rejected("ticket_expired");
        }
        let Some(mut stream) = self.stream.take() else {
            return rejected("bridge_unavailable");
        };
        let Ok(identity) = query_project(
            &mut stream,
            &self.target,
            &self.canonical_project_root,
            self.options,
        ) else {
            return rejected("target_changed");
        };
        if Instant::now() >= prepared.expires {
            self.identity = identity;
            self.stream = Some(stream);
            return rejected("ticket_expired");
        }
        let reply = query_data(
            &mut stream,
            json!({"type":COMMAND,"params":{"action":"apply","ticket":prepared.ticket}}),
            self.options,
        );
        let outcome = reply
            .as_ref()
            .map_or(GameObjectNameOutcome::Unknown, |data| {
                per_editor_outcome(data, &prepared)
            });
        if outcome != GameObjectNameOutcome::Unknown {
            self.identity = identity;
            self.stream = Some(stream);
        }
        outcome
    }
}

fn valid_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.encode_utf16().count() <= 256
        && !name.chars().any(char::is_control)
}

fn read_back_name(value: &Value) -> Option<&str> {
    value
        .as_str()
        .filter(|name| name.len() <= 4096 && !name.chars().any(char::is_control))
}

fn wire_integer(value: &Value, field: &str) -> Result<i32, BridgeError> {
    value
        .as_i64()
        .and_then(|value| i32::try_from(value).ok())
        .filter(|value| *value != 0)
        .ok_or_else(|| invalid_field(field))
}

fn rejected(reason: &'static str) -> GameObjectNameOutcome {
    GameObjectNameOutcome::NotExecuted { reason }
}

fn per_editor_outcome(data: &Value, prepared: &PreparedGameObjectName) -> GameObjectNameOutcome {
    match data["status"].as_str() {
        Some("notExecuted") => match data["reason"].as_str() {
            Some("ticket_invalid") => rejected("ticket_invalid"),
            Some("ticket_expired") => rejected("ticket_expired"),
            Some("target_changed") => rejected("target_changed"),
            _ => GameObjectNameOutcome::Unknown,
        },
        Some("mismatch")
            if data["sceneHandle"] == prepared.scene_handle
                && data["gameObjectInstanceId"] == prepared.game_object_instance_id =>
        {
            read_back_name(&data["actualName"]).map_or(GameObjectNameOutcome::Unknown, |name| {
                GameObjectNameOutcome::Mismatch {
                    actual_name: name.to_owned(),
                }
            })
        }
        Some("applied")
            if data["saved"] == false
                && data["sceneHandle"] == prepared.scene_handle
                && data["gameObjectInstanceId"] == prepared.game_object_instance_id
                && data["oldName"].as_str() == Some(&prepared.old_name)
                && data["newName"].as_str() == Some(&prepared.proposed_name) =>
        {
            GameObjectNameOutcome::Applied {
                actual_name: prepared.proposed_name.clone(),
            }
        }
        _ => GameObjectNameOutcome::Unknown,
    }
}
