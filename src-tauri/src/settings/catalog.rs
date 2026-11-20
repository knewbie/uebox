use std::{collections::BTreeMap, net::IpAddr};

use serde::{Deserialize, Serialize};
use url::{Host, Url};

const FORMAT: u32 = 1;
const MAX_PROFILES: usize = 128;
const INVALID: &str = "settings_invalid";

#[derive(Clone, Copy, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Purpose {
    Chat,
    Agent,
    Vision,
    Summary,
    Embedding,
    Image,
    Video,
    Model3d,
    Realtime,
    Tts,
    Stt,
    Music,
    Sfx,
    Search,
    Judge,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub(super) enum Authorization {
    None,
    #[serde(rename = "needsAuthorization")]
    Reauthorize,
    Keychain {
        reference: String,
    },
    Environment {
        name: String,
    },
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct ModelProfile {
    pub id: String,
    pub title: String,
    pub endpoint: String,
    pub model: String,
    pub authorization: Authorization,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_id: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Catalog {
    version: u32,
    pub profiles: Vec<ModelProfile>,
    pub bindings: BTreeMap<Purpose, String>,
}

impl Default for Catalog {
    fn default() -> Self {
        Self {
            version: FORMAT,
            profiles: Vec::new(),
            bindings: BTreeMap::new(),
        }
    }
}

pub(super) fn profile_id_valid(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

pub(super) fn environment_name_valid(name: &str) -> bool {
    name.len() <= 128
        && name.as_bytes().first().is_some_and(u8::is_ascii_uppercase)
        && name
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn display_value_valid(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

pub(super) fn endpoint(value: &str, secret: bool) -> Result<Url, &'static str> {
    if value.len() > 2048 || value.chars().any(char::is_control) {
        return Err(INVALID);
    }
    let endpoint = Url::parse(value).map_err(|_| INVALID)?;
    if endpoint.fragment().is_some()
        || endpoint.query().is_some()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
    {
        return Err(INVALID);
    }
    let local = match endpoint.host() {
        Some(Host::Ipv4(ip)) => IpAddr::V4(ip).is_loopback(),
        Some(Host::Ipv6(ip)) => IpAddr::V6(ip).is_loopback(),
        _ => false,
    };
    match endpoint.scheme() {
        "https" => Ok(endpoint),
        "http" if !secret || local => Ok(endpoint),
        _ => Err(INVALID),
    }
}

impl ModelProfile {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !profile_id_valid(&self.id)
            || !display_value_valid(&self.title, 256)
            || !display_value_valid(&self.model, 256)
            || self.title.trim() != self.title
            || self.model.trim() != self.model
        {
            return Err(INVALID);
        }
        endpoint(
            &self.endpoint,
            matches!(
                self.authorization,
                Authorization::Keychain { .. } | Authorization::Environment { .. }
            ),
        )?;
        if self.legacy_id.as_ref().is_some_and(|id| {
            id.len() != 64
                || !id
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        }) {
            return Err(INVALID);
        }
        match &self.authorization {
            Authorization::Keychain { reference } if !profile_id_valid(reference) => Err(INVALID),
            Authorization::Environment { name } if !environment_name_valid(name) => Err(INVALID),
            _ => Ok(()),
        }
    }
}

impl Catalog {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != FORMAT || self.profiles.len() > MAX_PROFILES {
            return Err(INVALID);
        }
        let mut profiles = std::collections::HashSet::new();
        let mut keys = std::collections::HashSet::new();
        let mut origins = std::collections::HashSet::new();
        for profile in &self.profiles {
            profile.validate()?;
            if !profiles.insert(profile.id.as_str()) {
                return Err(INVALID);
            }
            if let Authorization::Keychain { reference } = &profile.authorization {
                if !keys.insert(reference.as_str()) {
                    return Err(INVALID);
                }
            }
            if let Some(origin) = &profile.legacy_id {
                if !origins.insert(origin) {
                    return Err(INVALID);
                }
            }
        }
        if self
            .bindings
            .values()
            .any(|id| !profiles.contains(id.as_str()))
        {
            return Err(INVALID);
        }
        Ok(())
    }

    pub fn find(&self, id: &str) -> Option<&ModelProfile> {
        self.profiles.iter().find(|profile| profile.id == id)
    }

    pub fn put(&mut self, profile: ModelProfile) {
        if let Some(existing) = self
            .profiles
            .iter_mut()
            .find(|entry| entry.id == profile.id)
        {
            *existing = profile;
        } else {
            self.profiles.push(profile);
        }
    }

    pub fn remove(&mut self, id: &str) -> Option<ModelProfile> {
        let index = self.profiles.iter().position(|profile| profile.id == id)?;
        self.bindings.retain(|_, target| target != id);
        Some(self.profiles.remove(index))
    }
}
