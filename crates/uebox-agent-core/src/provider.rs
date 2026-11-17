use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use tokio::sync::watch;

const MAX_EVENT_BYTES: usize = 1024 * 1024;
const MAX_MODEL_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum Error {
    #[error("invalid provider configuration")]
    InvalidConfig,
    #[error("provider request failed")]
    Network,
    #[error("provider returned an unsuccessful HTTP status")]
    HttpStatus,
    #[error("invalid or incomplete provider stream")]
    Protocol,
    #[error("provider stopped at its output limit")]
    OutputLimit,
    #[error("provider reported an error")]
    Provider,
    #[error("run cancelled")]
    Cancelled,
    #[error("invalid tool definition or schema")]
    InvalidTool,
    #[error("tool belongs to a different engine")]
    WrongEngine,
    #[error("session already has an active run")]
    Busy,
    #[error("selected target is no longer current")]
    TargetChanged,
    #[error("model step limit reached")]
    StepLimit,
    #[error("request exceeds configured context byte limit")]
    ContextLimit,
    #[error("provider credential is unavailable")]
    CredentialUnavailable,
}

type TokenProvider = dyn Fn() -> Result<Option<String>, Error> + Send + Sync;
pub struct ProviderConfig {
    endpoint: String,
    model: String,
    bearer_token: Option<String>,
    max_output_tokens: Option<u32>,
    max_request_bytes: Option<usize>,
    headers: Vec<(String, String)>,
    token_provider: Option<Arc<TokenProvider>>,
}

impl ProviderConfig {
    pub fn new(endpoint: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            model: model.into(),
            bearer_token: None,
            max_output_tokens: None,
            max_request_bytes: None,
            headers: Vec::new(),
            token_provider: None,
        }
    }
    pub fn with_bearer_token(mut self, token: impl Into<String>) -> Self {
        self.bearer_token = Some(token.into());
        self
    }
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
    pub fn with_token_provider<F>(mut self, provider: F) -> Self
    where
        F: Fn() -> Result<Option<String>, Error> + Send + Sync + 'static,
    {
        self.token_provider = Some(Arc::new(provider));
        self
    }
    pub fn with_max_output_tokens(mut self, limit: u32) -> Self {
        self.max_output_tokens = Some(limit);
        self
    }
    pub fn with_max_request_bytes(mut self, limit: usize) -> Self {
        self.max_request_bytes = Some(limit);
        self
    }
}

struct CancellationState {
    cancelled: AtomicBool,
    signal: watch::Sender<bool>,
}

/// Cancellation ordered against event, dispatch and terminal admission.
/// Work admitted before `cancel` may be delivered after it returns; work that
/// loses admission to `cancel` is rejected. Callbacks run without a lock and
/// may call `cancel` themselves.
#[derive(Clone)]
pub struct Cancellation(Arc<CancellationState>);

/// A callback, dispatch, or commit that was admitted before cancellation.
pub(crate) struct Admission;
impl Admission {
    pub(crate) fn deliver<F: FnOnce() -> T, T>(self, callback: F) -> T {
        callback()
    }
}

impl Default for Cancellation {
    fn default() -> Self {
        Self::new()
    }
}

impl Cancellation {
    pub fn new() -> Self {
        Self(Arc::new(CancellationState {
            cancelled: AtomicBool::new(false),
            signal: watch::channel(false).0,
        }))
    }
    /// Mark cancellation without waiting for previously admitted work to finish.
    pub fn cancel(&self) {
        self.0.cancelled.swap(true, Ordering::SeqCst);
        self.0.signal.send_replace(true);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::SeqCst)
    }
    /// Linearization point shared with `cancel`; no lock is held during delivery.
    pub(crate) fn admit(&self) -> Result<Admission, Error> {
        self.0
            .cancelled
            .compare_exchange(false, false, Ordering::SeqCst, Ordering::SeqCst)
            .map(|_| Admission)
            .map_err(|_| Error::Cancelled)
    }
    pub(crate) fn settle<T>(&self, result: Result<T, Error>) -> Result<T, Error> {
        self.admit()?.deliver(|| result)
    }
    pub(crate) async fn cancelled(&self) {
        let mut receiver = self.0.signal.subscribe();
        loop {
            if self.is_cancelled() || *receiver.borrow_and_update() {
                return;
            }
            if receiver.changed().await.is_err() {
                return;
            }
        }
    }
}

#[derive(Clone, Debug)]
pub enum Message {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant(AssistantMessage),
    Tool {
        call_id: String,
        name: String,
        content: Value,
        is_error: bool,
    },
}
impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self::User {
            content: text.into(),
        }
    }
    fn wire(&self) -> Value {
        match self {
            Self::System { content } => json!({"role":"system", "content":content}),
            Self::User { content } => json!({"role":"user", "content":content}),
            Self::Assistant(message) => {
                let mut value = json!({"role":"assistant", "content": if message.text.is_empty() { Value::Null } else { json!(message.text) }});
                if !message.tool_calls.is_empty() {
                    value["tool_calls"] = json!(message.tool_calls.iter().map(|call| json!({"id":call.id,"type":"function","function":{"name":call.name,"arguments":call.arguments}})).collect::<Vec<_>>());
                }
                value
            }
            Self::Tool {
                call_id, content, ..
            } => json!({"role":"tool", "tool_call_id":call_id, "content":content.to_string()}),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelEvent {
    TextDelta(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssistantMessage {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
}

pub struct OpenAiProvider {
    client: reqwest::Client,
    endpoint: String,
    model: String,
    authorization: Option<reqwest::header::HeaderValue>,
    max_output_tokens: Option<u32>,
    max_request_bytes: Option<usize>,
    headers: reqwest::header::HeaderMap,
    token_provider: Option<Arc<TokenProvider>>,
}

fn authorization_header(token: &str) -> Result<reqwest::header::HeaderValue, Error> {
    if token.is_empty() {
        return Err(Error::InvalidConfig);
    }
    let mut value = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| Error::InvalidConfig)?;
    value.set_sensitive(true);
    Ok(value)
}

fn append_text_delta<F>(
    part: &str,
    text: &mut String,
    total_bytes: &mut usize,
    cancel: &Cancellation,
    events: &mut F,
) -> Result<(), Error>
where
    F: FnMut(ModelEvent),
{
    if part.is_empty() {
        return Ok(());
    }
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    *total_bytes = total_bytes.checked_add(part.len()).ok_or(Error::Protocol)?;
    if *total_bytes > MAX_MODEL_BYTES {
        return Err(Error::Protocol);
    }
    text.push_str(part);
    let event = ModelEvent::TextDelta(part.to_owned());
    cancel.admit()?.deliver(|| events(event));
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    Ok(())
}

impl OpenAiProvider {
    pub fn new(config: ProviderConfig) -> Result<Self, Error> {
        let url = reqwest::Url::parse(&config.endpoint).map_err(|_| Error::InvalidConfig)?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || config.model.trim().is_empty()
        {
            return Err(Error::InvalidConfig);
        }
        if config.max_output_tokens == Some(0) || config.max_request_bytes == Some(0) {
            return Err(Error::InvalidConfig);
        }
        if config.bearer_token.is_some() && config.token_provider.is_some() {
            return Err(Error::InvalidConfig);
        }
        let authorization = match config.bearer_token {
            None => None,
            Some(token) => Some(authorization_header(&token)?),
        };
        let mut headers = reqwest::header::HeaderMap::new();
        for (name, value) in config.headers {
            let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| Error::InvalidConfig)?;
            if matches!(
                name.as_str(),
                "authorization"
                    | "accept"
                    | "host"
                    | "content-length"
                    | "content-type"
                    | "transfer-encoding"
                    | "connection"
                    | "upgrade"
                    | "te"
                    | "trailer"
                    | "proxy-authorization"
                    | "proxy-connection"
            ) || headers.contains_key(&name)
            {
                return Err(Error::InvalidConfig);
            }
            let mut value =
                reqwest::header::HeaderValue::from_str(&value).map_err(|_| Error::InvalidConfig)?;
            value.set_sensitive(true);
            headers.insert(name, value);
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .map_err(|_| Error::InvalidConfig)?;
        Ok(Self {
            client,
            endpoint: config.endpoint,
            model: config.model,
            authorization,
            max_output_tokens: config.max_output_tokens,
            max_request_bytes: config.max_request_bytes,
            headers,
            token_provider: config.token_provider,
        })
    }

    pub async fn complete<F>(
        &self,
        messages: &[Message],
        tools: &[Value],
        cancel: &Cancellation,
        events: F,
    ) -> Result<AssistantMessage, Error>
    where
        F: FnMut(ModelEvent),
    {
        let result = self.complete_inner(messages, tools, cancel, events).await;
        cancel.settle(result)
    }

    async fn complete_inner<F>(
        &self,
        messages: &[Message],
        tools: &[Value],
        cancel: &Cancellation,
        mut events: F,
    ) -> Result<AssistantMessage, Error>
    where
        F: FnMut(ModelEvent),
    {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let mut body = json!({
            "model": self.model, "stream": true,
            "messages": messages.iter().map(Message::wire).collect::<Vec<_>>()
        });
        if !tools.is_empty() {
            body["tools"] = json!(tools);
        }
        if let Some(limit) = self.max_output_tokens {
            body["max_tokens"] = json!(limit);
        }
        let body = serde_json::to_vec(&body).map_err(|_| Error::InvalidConfig)?;
        if self
            .max_request_bytes
            .is_some_and(|limit| body.len() > limit)
        {
            return Err(Error::ContextLimit);
        }
        let authorization = if let Some(provider) = &self.token_provider {
            match provider().map_err(|_| Error::CredentialUnavailable)? {
                Some(token) => {
                    Some(authorization_header(&token).map_err(|_| Error::CredentialUnavailable)?)
                }
                None => None,
            }
        } else {
            self.authorization.clone()
        };
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let mut request = self
            .client
            .post(&self.endpoint)
            .headers(self.headers.clone())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .body(body);
        if let Some(value) = authorization {
            request = request.header(reqwest::header::AUTHORIZATION, value);
        }
        let mut response = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(Error::Cancelled),
            result = request.send() => result.map_err(|_| Error::Network)?,
        };
        if !response.status().is_success() {
            return Err(Error::HttpStatus);
        }
        let mut parser = Sse::default();
        let mut text = String::new();
        let mut finish: Option<String> = None;
        let mut calls: BTreeMap<u64, ToolCall> = BTreeMap::new();
        let mut total_bytes = 0usize;
        let mut saw_refusal = false;
        let mut saw_refusal_field = false;
        loop {
            let chunk = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(Error::Cancelled),
                result = response.chunk() => result.map_err(|_| Error::Network)?,
            };
            let Some(chunk) = chunk else {
                return Err(Error::Protocol);
            };
            for data in parser.push(&chunk)? {
                if cancel.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                if data == "[DONE]" {
                    let tool_calls: Vec<_> = calls.into_values().collect();
                    let mut ids = HashSet::new();
                    if tool_calls.iter().any(|call| {
                        call.id.is_empty() || call.name.is_empty() || !ids.insert(&call.id)
                    }) {
                        return Err(Error::Protocol);
                    }
                    if saw_refusal_field && text.is_empty() && tool_calls.is_empty() {
                        return Err(Error::Protocol);
                    }
                    return match finish.as_deref() {
                        Some("stop") if tool_calls.is_empty() => {
                            Ok(AssistantMessage { text, tool_calls })
                        }
                        Some("tool_calls") if !tool_calls.is_empty() => {
                            Ok(AssistantMessage { text, tool_calls })
                        }
                        _ => Err(Error::Protocol),
                    };
                }
                let event: Value = serde_json::from_str(&data).map_err(|_| Error::Protocol)?;
                if event.get("error").is_some() {
                    return Err(Error::Provider);
                }
                if let Some(choices) = event.get("choices").and_then(Value::as_array) {
                    for choice in choices {
                        if finish.is_some() {
                            return Err(Error::Protocol);
                        }
                        if choice.get("index").and_then(Value::as_u64) != Some(0) {
                            return Err(Error::Protocol);
                        }
                        let delta = choice
                            .get("delta")
                            .and_then(Value::as_object)
                            .ok_or(Error::Protocol)?;
                        if delta.get("function_call").is_some() {
                            return Err(Error::Protocol);
                        }
                        if let Some(content) = delta.get("content").filter(|value| !value.is_null())
                        {
                            let part = content.as_str().ok_or(Error::Protocol)?;
                            append_text_delta(
                                part,
                                &mut text,
                                &mut total_bytes,
                                cancel,
                                &mut events,
                            )?;
                        }
                        if let Some(refusal) = delta.get("refusal").filter(|value| !value.is_null())
                        {
                            let part = refusal.as_str().ok_or(Error::Protocol)?;
                            saw_refusal_field = true;
                            saw_refusal |= !part.is_empty();
                            append_text_delta(
                                part,
                                &mut text,
                                &mut total_bytes,
                                cancel,
                                &mut events,
                            )?;
                        }
                        if let Some(tool_calls) = delta.get("tool_calls") {
                            for fragment in tool_calls.as_array().ok_or(Error::Protocol)? {
                                let index = fragment
                                    .get("index")
                                    .and_then(Value::as_u64)
                                    .ok_or(Error::Protocol)?;
                                if index >= 128 {
                                    return Err(Error::Protocol);
                                }
                                if let Some(kind) = fragment.get("type") {
                                    if kind.as_str() != Some("function") {
                                        return Err(Error::Protocol);
                                    }
                                }
                                let call = calls.entry(index).or_insert_with(|| ToolCall {
                                    id: String::new(),
                                    name: String::new(),
                                    arguments: String::new(),
                                });
                                if let Some(id) = fragment.get("id") {
                                    let id = id.as_str().ok_or(Error::Protocol)?;
                                    if id.is_empty() {
                                        return Err(Error::Protocol);
                                    }
                                    if call.id.is_empty() {
                                        call.id.push_str(id);
                                    } else if call.id != id {
                                        return Err(Error::Protocol);
                                    }
                                    total_bytes += id.len();
                                }
                                if let Some(function) = fragment.get("function") {
                                    let function = function.as_object().ok_or(Error::Protocol)?;
                                    if let Some(name) = function.get("name") {
                                        let name = name.as_str().ok_or(Error::Protocol)?;
                                        call.name.push_str(name);
                                        total_bytes += name.len();
                                    }
                                    if let Some(arguments) = function.get("arguments") {
                                        let arguments =
                                            arguments.as_str().ok_or(Error::Protocol)?;
                                        call.arguments.push_str(arguments);
                                        total_bytes += arguments.len();
                                    }
                                }
                            }
                        }
                        if total_bytes > MAX_MODEL_BYTES || (saw_refusal && !calls.is_empty()) {
                            return Err(Error::Protocol);
                        }
                        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                            match reason {
                                "stop" | "tool_calls" => finish = Some(reason.to_owned()),
                                "length" => return Err(Error::OutputLimit),
                                _ => return Err(Error::Provider),
                            }
                        }
                    }
                } else {
                    return Err(Error::Protocol);
                }
            }
        }
    }
}

#[derive(Default)]
struct Sse {
    line: Vec<u8>,
    data: Vec<u8>,
    skip_lf: bool,
    seen_first_line: bool,
}
impl Sse {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, Error> {
        let mut output = Vec::new();
        for &byte in bytes {
            if self.skip_lf {
                self.skip_lf = false;
                if byte == b'\n' {
                    continue;
                }
            }
            if byte == b'\r' || byte == b'\n' {
                self.skip_lf = byte == b'\r';
                self.line(&mut output)?;
            } else {
                self.line.push(byte);
                if self.line.len() + self.data.len() > MAX_EVENT_BYTES {
                    return Err(Error::Protocol);
                }
            }
        }
        Ok(output)
    }
    fn line(&mut self, output: &mut Vec<String>) -> Result<(), Error> {
        let mut line = std::mem::take(&mut self.line);
        if !self.seen_first_line {
            self.seen_first_line = true;
            if line.starts_with(b"\xef\xbb\xbf") {
                line.drain(..3);
            }
        }
        std::str::from_utf8(&line).map_err(|_| Error::Protocol)?;
        if line.is_empty() {
            if !self.data.is_empty() {
                self.data.pop();
                output.push(
                    String::from_utf8(std::mem::take(&mut self.data))
                        .map_err(|_| Error::Protocol)?,
                );
            }
        } else if line[0] != b':' {
            let separator = line.iter().position(|&b| b == b':').unwrap_or(line.len());
            if &line[..separator] == b"data" {
                let mut value = if separator < line.len() {
                    &line[separator + 1..]
                } else {
                    &[]
                };
                if value.first() == Some(&b' ') {
                    value = &value[1..];
                }
                self.data.extend_from_slice(value);
                self.data.push(b'\n');
                if self.data.len() > MAX_EVENT_BYTES {
                    return Err(Error::Protocol);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Cancellation, Error, Sse};

    #[test]
    fn sse_accepts_a_split_bom_and_lone_cr_delimiters() {
        let input = b"\xef\xbb\xbfdata: first\r: ping\rdata: second\r\r";
        let mut parser = Sse::default();
        let mut events = Vec::new();
        for byte in input {
            events.extend(parser.push(&[*byte]).unwrap());
        }
        assert_eq!(events, vec!["first\nsecond"]);
    }

    #[test]
    fn sse_rejects_invalid_utf8_even_in_an_ignored_field() {
        let mut parser = Sse::default();
        assert_eq!(parser.push(b": \xff\n\n").unwrap_err(), Error::Protocol);
    }

    #[test]
    fn cross_thread_cancel_orders_event_admission_without_retracting_an_admitted_callback() {
        let cancellation = Cancellation::new();
        let admitted = cancellation.admit().unwrap();
        let other = cancellation.clone();
        std::thread::spawn(move || other.cancel()).join().unwrap();
        let mut delivered = false;
        admitted.deliver(|| {
            assert!(cancellation.is_cancelled());
            delivered = true;
        });
        assert!(delivered);
        assert!(matches!(cancellation.admit(), Err(Error::Cancelled)));
    }

    #[test]
    fn cross_thread_cancel_orders_terminal_admission_not_caller_observation() {
        let cancellation = Cancellation::new();
        let worker_cancel = cancellation.clone();
        let (admitted_tx, admitted_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result: Result<&str, Error> = worker_cancel.settle(Ok("completed"));
            admitted_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            result
        });
        admitted_rx.recv().unwrap();
        cancellation.cancel();
        release_tx.send(()).unwrap();
        assert_eq!(worker.join().unwrap(), Ok("completed"));
        let later: Result<&str, Error> = cancellation.settle(Ok("late"));
        assert_eq!(later, Err(Error::Cancelled));
    }
}
