//! DeepSeek Harness (DSH) Web API 客户端。
//!
//! DSH 不是 OpenAI 兼容模型接口，而是一个 Agent runtime：普通请求走
//! `/api/<method>` 的 JSON RPC，回合事件走只读 WebSocket。这个模块只处理
//! 传输、信封与基础回合收集，不参与 Nonoka 的界面、平台和权限决策。

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use reqwest::Client;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type DshSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

const DEFAULT_BASE_URL: &str = "http://127.0.0.1:3080";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct DshClient {
    base_url: String,
    http: Client,
    timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DshSession {
    pub session_id: String,
    pub workspace_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DshPrompt {
    pub session_id: String,
    pub mode: String,
    pub content: Vec<DshContentBlock>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum DshContentBlock {
    Text {
        text: String,
    },
    Image {
        #[serde(rename = "mediaType")]
        media_type: String,
        data: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct DshEventFrame {
    pub rpc_id: String,
    pub kind: String,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DshTurnResult {
    pub session_id: String,
    pub text: String,
    pub reason: Value,
}

pub struct DshEventStream {
    socket: DshSocket,
    endpoint: String,
}

pub struct DshTurnCollector {
    session_id: String,
    text: String,
    active_turn: Option<Value>,
}

impl DshClient {
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        Self::with_timeout(base_url, DEFAULT_TIMEOUT)
    }

    pub fn from_env() -> Result<Self> {
        Self::new(
            std::env::var("NONOKA_DSH_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string()),
        )
    }

    pub fn with_timeout(base_url: impl Into<String>, timeout: Duration) -> Result<Self> {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
            bail!("DSH base URL must start with http:// or https://");
        }
        Ok(Self {
            base_url,
            http: Client::builder()
                .connect_timeout(timeout)
                .build()
                .context("building DSH HTTP client")?,
            timeout,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub async fn host_describe(&self) -> Result<Value> {
        self.call("host.describe", json!({})).await
    }

    pub async fn workspace_create(&self, path: impl Into<String>) -> Result<Value> {
        self.call("workspace.create", json!({ "path": path.into() }))
            .await
    }

    pub async fn session_create(
        &self,
        cwd: Option<&str>,
        workspace_id: Option<&str>,
        agent_preset: Option<&str>,
    ) -> Result<DshSession> {
        if cwd.is_some() && workspace_id.is_some() {
            bail!("DSH session.create accepts cwd or workspaceId, not both");
        }
        let mut payload = Map::new();
        if let Some(cwd) = cwd {
            payload.insert("cwd".to_string(), Value::String(cwd.to_string()));
        }
        if let Some(workspace_id) = workspace_id {
            payload.insert(
                "workspaceId".to_string(),
                Value::String(workspace_id.to_string()),
            );
        }
        if let Some(agent_preset) = agent_preset {
            payload.insert(
                "agentPreset".to_string(),
                Value::String(agent_preset.to_string()),
            );
        }
        let value = self.call("session.create", Value::Object(payload)).await?;
        let session_id = value
            .get("sessionId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .context("DSH session.create returned no sessionId")?;
        Ok(DshSession {
            session_id: session_id.to_string(),
            workspace_id: None,
        })
    }

    pub async fn session_prompt(&self, prompt: &DshPrompt) -> Result<Value> {
        self.call("session.prompt", serde_json::to_value(prompt)?)
            .await
    }

    pub async fn events_mux(&self) -> Result<DshEventStream> {
        let endpoint = websocket_url(&self.base_url, "/api/events.mux")?;
        let (socket, _) = connect_async(&endpoint)
            .await
            .with_context(|| format!("connecting to DSH event stream {endpoint}"))?;
        Ok(DshEventStream { socket, endpoint })
    }

    async fn call(&self, method: &str, payload: Value) -> Result<Value> {
        let rpc_id = new_rpc_id();
        let request = json!({
            "type": "client-request",
            "rpcId": rpc_id,
            "method": method,
            "payload": payload,
        });
        let response = self
            .http
            .post(format!("{}/api/{method}", self.base_url))
            .timeout(self.timeout)
            .header("content-type", "application/json")
            .json(&request)
            .send()
            .await
            .with_context(|| format!("sending DSH RPC {method}"))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            bail!("DSH RPC {method} returned HTTP {status}: {body}");
        }
        let response: Value = response
            .json()
            .await
            .with_context(|| format!("decoding DSH RPC {method} response"))?;
        if response.get("type").and_then(Value::as_str) != Some("server-response") {
            bail!("DSH RPC {method} returned an invalid response envelope");
        }
        if response.get("rpcId").and_then(Value::as_str) != Some(rpc_id.as_str()) {
            bail!("DSH RPC {method} returned a mismatched rpcId");
        }
        let result = response
            .get("result")
            .context("DSH response has no result")?;
        if result.get("ok").and_then(Value::as_bool) != Some(true) {
            let error = result.get("error").cloned().unwrap_or(Value::Null);
            let code = error
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown DSH error");
            bail!("DSH RPC {method} failed: {code}: {message}");
        }
        Ok(result.get("value").cloned().unwrap_or(Value::Null))
    }
}

impl DshEventStream {
    /// 等 DSH 明确确认该 session 已挂到 mux 流上再投递 prompt。
    ///
    /// DSH 会在新连接建立后补发所有既有 session 的 `session/subscribed`；只等
    /// WebSocket handshake 完成并不代表服务端已经把新 session 加进订阅集合。
    pub async fn wait_for_session_subscription(&mut self, session_id: &str) -> Result<()> {
        loop {
            let frame = self
                .next_session_frame(session_id)
                .await?
                .context("DSH event stream closed before session subscription")?;
            if frame.kind == "session/subscribed" {
                return Ok(());
            }
            if frame.kind == "stream/error" {
                bail!("DSH event stream error: {}", frame.payload);
            }
        }
    }

    /// 读取任意 DSH mux 帧。只在确实需要监听全局宿主状态时使用。
    pub async fn next_frame(&mut self) -> Result<Option<DshEventFrame>> {
        self.next_matching_frame(None).await
    }

    /// 读取指定 DSH session 的帧。
    ///
    /// DSH mux 是全局广播流，其他会话可能有数 MB 的工具结果。先在文本层
    /// 过滤 session id，避免为无关会话做 JSON 解析、分配和日志格式化。
    pub async fn next_session_frame(&mut self, session_id: &str) -> Result<Option<DshEventFrame>> {
        self.next_matching_frame(Some(session_id)).await
    }

    async fn next_matching_frame(
        &mut self,
        session_id: Option<&str>,
    ) -> Result<Option<DshEventFrame>> {
        while let Some(message) = self.socket.next().await {
            let message = message.context("reading DSH event WebSocket")?;
            let text = match message {
                Message::Text(text) => text,
                Message::Binary(_) => {
                    tracing::warn!(endpoint = %self.endpoint, "dropping binary DSH event frame");
                    continue;
                }
                Message::Ping(_) | Message::Pong(_) => continue,
                Message::Close(_) => return Ok(None),
                _ => continue,
            };
            if session_id.is_some_and(|session_id| {
                !text.contains(session_id) && !text.contains("\"type\":\"stream/error\"")
            }) {
                continue;
            }
            let envelope: Value = match serde_json::from_str(&text) {
                Ok(value) => value,
                Err(error) => {
                    tracing::warn!(endpoint = %self.endpoint, %error, "dropping malformed DSH event JSON");
                    continue;
                }
            };
            if envelope.get("type").and_then(Value::as_str) != Some("server-request") {
                tracing::warn!(endpoint = %self.endpoint, "dropping non-server-request DSH event frame");
                continue;
            }
            let rpc_id = match envelope.get("rpcId").and_then(Value::as_str) {
                Some(value) => value.to_string(),
                None => {
                    tracing::warn!(endpoint = %self.endpoint, "dropping DSH event without rpcId");
                    continue;
                }
            };
            let kind = match envelope.get("method").and_then(Value::as_str) {
                Some(value) => value.to_string(),
                None => {
                    tracing::warn!(endpoint = %self.endpoint, "dropping DSH event without method");
                    continue;
                }
            };
            let payload = envelope.get("payload").cloned().unwrap_or(Value::Null);
            if let Some(session_id) = session_id {
                if kind != "stream/error"
                    && payload.get("sessionId").and_then(Value::as_str) != Some(session_id)
                {
                    continue;
                }
            }
            return Ok(Some(DshEventFrame {
                rpc_id,
                kind,
                payload,
            }));
        }
        Ok(None)
    }
}

impl DshTurnCollector {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            text: String::new(),
            active_turn: None,
        }
    }

    pub fn push(&mut self, frame: &DshEventFrame) -> Result<Option<DshTurnResult>> {
        if frame.kind != "session/event" {
            if frame.kind == "stream/error" {
                bail!("DSH event stream error: {}", frame.payload);
            }
            return Ok(None);
        }
        if frame.payload.get("sessionId").and_then(Value::as_str) != Some(self.session_id.as_str())
        {
            return Ok(None);
        }
        let event = frame
            .payload
            .get("event")
            .context("DSH session/event has no event")?;
        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match event_type {
            "turn/start" => {
                self.text.clear();
                self.active_turn = event.get("data").and_then(|data| data.get("turn")).cloned();
            }
            "assistant/message" => self.append_assistant_message(event),
            "turn/end" => {
                let reason = event
                    .get("data")
                    .and_then(|data| data.get("reason"))
                    .cloned()
                    .unwrap_or(Value::Null);
                return Ok(Some(DshTurnResult {
                    session_id: self.session_id.clone(),
                    text: std::mem::take(&mut self.text),
                    reason,
                }));
            }
            _ => {}
        }
        Ok(None)
    }

    fn append_assistant_message(&mut self, event: &Value) {
        let Some(content) = event
            .get("data")
            .and_then(|data| data.get("message"))
            .and_then(|message| message.get("content"))
            .and_then(Value::as_array)
        else {
            return;
        };
        for block in content {
            if block.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    self.text.push_str(text);
                }
            }
        }
    }
}

fn websocket_url(base_url: &str, path: &str) -> Result<String> {
    let scheme = if base_url.starts_with("https://") {
        "wss://"
    } else {
        "ws://"
    };
    let authority = base_url
        .strip_prefix("https://")
        .or_else(|| base_url.strip_prefix("http://"))
        .context("invalid DSH base URL")?;
    Ok(format!("{scheme}{authority}{path}"))
}

fn new_rpc_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("nonoka-{}-{nanos}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(payload: Value) -> DshEventFrame {
        DshEventFrame {
            rpc_id: "event-1".to_string(),
            kind: "session/event".to_string(),
            payload,
        }
    }

    #[test]
    fn websocket_url_follows_http_scheme() {
        assert_eq!(
            websocket_url("http://127.0.0.1:3080", "/api/events.mux").unwrap(),
            "ws://127.0.0.1:3080/api/events.mux"
        );
        assert_eq!(
            websocket_url("https://example.test/", "/api/events.mux").unwrap(),
            "wss://example.test//api/events.mux"
        );
    }

    #[test]
    fn collector_only_finishes_for_the_target_session() {
        let mut collector = DshTurnCollector::new("sess-1");
        assert!(collector
            .push(&frame(json!({
                "sessionId": "sess-2",
                "event": {"type": "turn/end", "data": {"reason": {"kind": "done"}}}
            })))
            .unwrap()
            .is_none());
        collector
            .push(&frame(json!({
                "sessionId": "sess-1",
                "event": {"type": "turn/start", "data": {"turn": 1}}
            })))
            .unwrap();
        collector
            .push(&frame(json!({
                "sessionId": "sess-1",
                "event": {"type": "assistant/message", "data": {"message": {"content": [{"type": "text", "text": "你好"}]}}}
            })))
            .unwrap();
        let result = collector
            .push(&frame(json!({
                "sessionId": "sess-1",
                "event": {"type": "turn/end", "data": {"reason": {"kind": "done"}}}
            })))
            .unwrap()
            .unwrap();
        assert_eq!(result.text, "你好");
        assert_eq!(result.session_id, "sess-1");
    }

    #[test]
    fn prompt_serializes_dsh_content_shape() {
        let prompt = DshPrompt {
            session_id: "sess-1".to_string(),
            mode: "queue".to_string(),
            content: vec![DshContentBlock::Text {
                text: "你好".to_string(),
            }],
        };
        let value = serde_json::to_value(prompt).unwrap();
        assert_eq!(value["sessionId"], "sess-1");
        assert_eq!(value["content"][0]["type"], "text");
    }
}
