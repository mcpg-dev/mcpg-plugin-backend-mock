//! Mock backend binding plugin for mcpg (`kind: "mock"`).
//!
//! A developer/test fixture: returns an operator-configured response
//! with no external I/O. Three modes:
//!
//! - `error: true` — a simulated tool-level error (`is_error: true`).
//! - `passthrough: true` — `response` is a literal `CallToolResult`
//!   surfaced unchanged (image / audio / embedded-resource / mixed
//!   content the wrapping path can't reach).
//! - default — `response` is JSON-stringified into a text content block
//!   plus structured metadata.
//!
//! The plain plugin payload→envelope contract can't express an
//! operator-controlled `is_error` or a literal multi-block `content`
//! array, so this plugin emits its result under the host's
//! verbatim-result envelope convention: a single object
//! `{ "<VERBATIM_RESULT_KEY>": <CallToolResult> }`. The gateway projects
//! that `CallToolResult` directly onto `tools/call` (content + is_error
//! verbatim), falling back to the standard projection if absent.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mcpg_plugin_protocol::{
    BackendError, BackendHost, BackendPlugin, BackendRequest, BackendResponse, PluginManifest,
    firstparty_manifest,
};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::RwLock;

pub mod cdylib;

/// Embedded plugin descriptor — passed to the gateway registrar at
/// startup.
pub const BINDING_DESCRIPTOR_YAML: &str = include_str!("../plugin.yaml");

/// Manifest id this plugin registers under, matching `id:` in
/// [`BINDING_DESCRIPTOR_YAML`]. Exported so the gateway can tell whether an
/// operator has already declared this artefact in `plugins[]` before it
/// falls back to the statically-linked copy — registering both would
/// collide on the alias.
pub const PLUGIN_ID: &str = "dev.mcpg.backend.mock";

/// Envelope sentinel: when a plugin's response payload is a single
/// object carrying this key, the gateway deserializes the value into a
/// `ToolCallResult` and projects it onto `tools/call` verbatim — content
/// and `is_error` operator-controlled. The gateway recognizes the same
/// literal in `execute_envelope_plugin`.
pub const VERBATIM_RESULT_KEY: &str = "__mcpg_verbatim_result";

/// Operator-facing spec the gateway serializes when calling
/// `register_profile`. Mirrors `MockBackendConfig` in the gateway crate.
#[derive(Debug, Clone, Deserialize)]
struct MockBackendSpec {
    #[serde(default)]
    response: Value,
    #[serde(default)]
    delay_ms: u64,
    #[serde(default)]
    error: bool,
    #[serde(default)]
    error_message: Option<String>,
    #[serde(default)]
    passthrough: bool,
}

#[derive(Clone)]
struct MockProfile {
    response: Value,
    delay_ms: u64,
    error: bool,
    error_message: Option<String>,
    passthrough: bool,
}

/// `BackendPlugin` implementation for `kind: "mock"`.
pub struct MockBackendPlugin {
    manifest: PluginManifest,
    profiles: RwLock<BTreeMap<String, MockProfile>>,
}

impl Default for MockBackendPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl MockBackendPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            manifest: firstparty_manifest! {
                id: "dev.mcpg.backend.mock",
                name: "Mock Binding",
                class: Backend,
            },
            profiles: RwLock::new(BTreeMap::new()),
        }
    }
}

impl std::fmt::Debug for MockBackendPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockBackendPlugin")
            .field("id", &self.manifest.id)
            .finish()
    }
}

#[async_trait]
impl BackendPlugin for MockBackendPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn kind(&self) -> &str {
        "mock"
    }

    async fn register_profile(
        &self,
        backend_name: &str,
        spec: &serde_json::Value,
        _host: Arc<dyn BackendHost>,
    ) -> Result<(), BackendError> {
        let parsed: MockBackendSpec =
            serde_json::from_value(spec.clone()).map_err(|e| BackendError::InvalidSpec {
                message: format!("mock binding spec: {e}"),
            })?;
        // Passthrough requires a CallToolResult-shaped object (a
        // `content` array); fail fast at registration, mirroring the
        // gateway's MockBackendConfig::validate.
        if parsed.passthrough && !is_call_tool_result_shape(&parsed.response) {
            return Err(BackendError::InvalidSpec {
                message: "passthrough: true requires `response` to be a CallToolResult object \
                          with a `content` array"
                    .into(),
            });
        }
        self.profiles.write().await.insert(
            backend_name.to_owned(),
            MockProfile {
                response: parsed.response,
                delay_ms: parsed.delay_ms,
                error: parsed.error,
                error_message: parsed.error_message,
                passthrough: parsed.passthrough,
            },
        );
        Ok(())
    }

    async fn execute(
        &self,
        backend_name: &str,
        request: BackendRequest,
    ) -> Result<BackendResponse, BackendError> {
        let profile = {
            let guard = self.profiles.read().await;
            guard
                .get(backend_name)
                .cloned()
                .ok_or_else(|| BackendError::ProfileNotFound {
                    backend_name: backend_name.to_owned(),
                })?
        };

        if profile.delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(profile.delay_ms)).await;
        }

        let arguments: Value = if request.payload.is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_slice(&request.payload).unwrap_or_else(|_| serde_json::json!({}))
        };
        let tool_name = request
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("mcpg-tool-name"))
            .map(|(_, v)| v.as_str())
            .unwrap_or(backend_name)
            .to_owned();

        let result = build_mock_result(&profile, &tool_name, backend_name, &arguments);
        // Wrap under the verbatim-result sentinel so the gateway projects
        // the CallToolResult (content + is_error) verbatim.
        let envelope = serde_json::json!({ VERBATIM_RESULT_KEY: result });
        let payload = serde_json::to_vec(&envelope).unwrap_or_else(|_| b"{}".to_vec());
        Ok(BackendResponse {
            payload,
            truncated: false,
        })
    }

    fn audit_metadata(&self, _backend_name: &str) -> serde_json::Map<String, serde_json::Value> {
        let mut map = serde_json::Map::new();
        map.insert("mock.transport".to_owned(), serde_json::json!("plugin"));
        map
    }
}

/// A loose CallToolResult shape check: an object carrying a `content`
/// array. Used to gate `passthrough` (the operator's `response` must be
/// a literal CallToolResult).
fn is_call_tool_result_shape(value: &Value) -> bool {
    value.get("content").map(|c| c.is_array()).unwrap_or(false)
}

/// A single text content block in MCP `ToolContent` wire shape.
fn text_content(text: String) -> Value {
    serde_json::json!({ "type": "text", "text": text })
}

/// Build the `CallToolResult` JSON for this mock profile + call across
/// the error / passthrough / default modes.
fn build_mock_result(
    profile: &MockProfile,
    tool_name: &str,
    backend_name: &str,
    arguments: &Value,
) -> Value {
    if profile.error {
        let error_msg = profile
            .error_message
            .clone()
            .unwrap_or_else(|| "mock error".to_owned());
        return serde_json::json!({
            "content": [text_content(error_msg.clone())],
            "structuredContent": {
                "toolName": tool_name,
                "profile": backend_name,
                "bindingKind": "mock",
                "arguments": arguments,
                "error": error_msg,
                "simulated": true,
            },
            "isError": true,
        });
    }

    if profile.passthrough {
        // `response` is a literal CallToolResult (validated at register
        // time). Surface it unchanged.
        return profile.response.clone();
    }

    serde_json::json!({
        "content": [text_content(
            serde_json::to_string_pretty(&profile.response)
                .unwrap_or_else(|_| profile.response.to_string()),
        )],
        "structuredContent": {
            "toolName": tool_name,
            "profile": backend_name,
            "bindingKind": "mock",
            "arguments": arguments,
            "response": profile.response,
            "delayMs": profile.delay_ms,
            "simulated": true,
        },
        "isError": false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_id_matches_the_descriptor() {
        // The gateway compares config `plugins[]` entries against PLUGIN_ID to
        // decide whether to skip its statically-linked fallback. If the two
        // drift, that check silently stops matching and both copies register.
        let declared = BINDING_DESCRIPTOR_YAML
            .lines()
            .find_map(|l| l.strip_prefix("id:"))
            .map(str::trim)
            .expect("descriptor declares an id");
        assert_eq!(declared, PLUGIN_ID);
    }

    fn req(args: Value) -> BackendRequest {
        BackendRequest {
            payload: serde_json::to_vec(&args).unwrap(),
            headers: vec![("mcpg-tool-name".to_owned(), "t".to_owned())],
            request_id: "rq-1".into(),
            session_id: None,
            identity: None,
            idempotency: None,
        }
    }

    fn verbatim(resp: &BackendResponse) -> Value {
        let env: Value = serde_json::from_slice(&resp.payload).unwrap();
        env[VERBATIM_RESULT_KEY].clone()
    }

    #[test]
    fn binding_plugin_kind_is_mock() {
        assert_eq!(MockBackendPlugin::new().kind(), "mock");
    }

    #[tokio::test]
    async fn default_mode_wraps_response_as_text() {
        let plugin = MockBackendPlugin::new();
        plugin
            .register_profile(
                "m",
                &serde_json::json!({ "response": {"ok": true} }),
                mcpg_plugin_protocol::noop_backend_host(),
            )
            .await
            .expect("register");
        let resp = plugin
            .execute("m", req(serde_json::json!({"a": 1})))
            .await
            .unwrap();
        let r = verbatim(&resp);
        assert_eq!(r["isError"], false);
        assert_eq!(r["structuredContent"]["response"]["ok"], true);
        assert_eq!(r["content"][0]["type"], "text");
    }

    #[tokio::test]
    async fn error_mode_sets_is_error() {
        let plugin = MockBackendPlugin::new();
        plugin
            .register_profile(
                "m",
                &serde_json::json!({ "error": true, "error_message": "boom" }),
                mcpg_plugin_protocol::noop_backend_host(),
            )
            .await
            .expect("register");
        let resp = plugin
            .execute("m", req(serde_json::json!({})))
            .await
            .unwrap();
        let r = verbatim(&resp);
        assert_eq!(r["isError"], true);
        assert_eq!(r["content"][0]["text"], "boom");
    }

    #[tokio::test]
    async fn passthrough_surfaces_literal_result() {
        let plugin = MockBackendPlugin::new();
        let literal = serde_json::json!({
            "content": [{"type": "image", "data": "abc", "mimeType": "image/png"}],
            "isError": false,
        });
        plugin
            .register_profile(
                "m",
                &serde_json::json!({ "passthrough": true, "response": literal }),
                mcpg_plugin_protocol::noop_backend_host(),
            )
            .await
            .expect("register");
        let resp = plugin
            .execute("m", req(serde_json::json!({})))
            .await
            .unwrap();
        let r = verbatim(&resp);
        assert_eq!(r["content"][0]["type"], "image");
        assert_eq!(r["content"][0]["mimeType"], "image/png");
    }

    #[tokio::test]
    async fn passthrough_rejects_non_call_tool_result() {
        let plugin = MockBackendPlugin::new();
        let err = plugin
            .register_profile(
                "m",
                &serde_json::json!({ "passthrough": true, "response": {"not": "a result"} }),
                mcpg_plugin_protocol::noop_backend_host(),
            )
            .await
            .expect_err("non-CallToolResult passthrough rejected");
        assert!(matches!(err, BackendError::InvalidSpec { .. }));
    }
}
