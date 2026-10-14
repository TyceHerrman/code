use crate::config::find_codex_home;
use crate::context_manager::truncate_function_output_payload;
use crate::original_image_detail::sanitize_original_image_detail;
use crate::session::session::Session;
use crate::session::step_context::StepContext;
use crate::session::turn_context::TurnContext;
use crate::tools::TELEMETRY_PREVIEW_MAX_BYTES;
use crate::tools::TELEMETRY_PREVIEW_MAX_LINES;
use crate::tools::TELEMETRY_PREVIEW_TRUNCATION_NOTICE;
use crate::turn_diff_tracker::TurnDiffTracker;
use crate::unified_exec::format_output_omission_marker;
use crate::unified_exec::resolve_max_tokens;
use codex_protocol::mcp::CallToolResult;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::models::function_call_output_content_items_to_text;
use codex_tools::LoadableToolSpec;
use codex_tools::ToolName;
use codex_utils_output_truncation::TruncationPolicy;
use codex_utils_output_truncation::approx_token_count;
use codex_utils_output_truncation::formatted_truncate_text;
use codex_utils_output_truncation::truncate_text;
use codex_utils_string::take_bytes_at_char_boundary;
use codex_zero_codec::append_telemetry_to_path;
use codex_zero_codec::encode_command_output;
use codex_zero_codec::select_strictly_smaller;
use codex_zero_codec::store_raw;
use serde::Serialize;
use serde_json::Value as JsonValue;
use std::num::NonZeroUsize;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub use codex_tools::ToolOutput;
pub use codex_tools::ToolPayload;

pub(crate) fn boxed_tool_output<T>(output: T) -> Box<dyn ToolOutput>
where
    T: ToolOutput + 'static,
{
    Box::new(output)
}

pub type SharedTurnDiffTracker = Arc<Mutex<TurnDiffTracker>>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ToolCallSource {
    Direct,
    DirectPlaintextMessage,
    CodeMode {
        /// Runtime cell that issued the nested tool request.
        cell_id: String,
        /// Code-mode's per-cell tool invocation id. This is useful for
        /// debugging the JS/runtime bridge, but it is not the Codex tool call id
        /// because the runtime id only needs to be unique within one cell.
        runtime_tool_call_id: String,
    },
}

#[derive(Clone)]
pub struct ToolInvocation {
    pub session: Arc<Session>,
    // TODO(sayan): Remove this compatibility field once handlers use `step_context.turn`.
    pub turn: Arc<TurnContext>,
    pub(crate) step_context: Arc<StepContext>,
    pub cancellation_token: CancellationToken,
    pub tracker: SharedTurnDiffTracker,
    pub call_id: String,
    pub tool_name: ToolName,
    pub source: ToolCallSource,
    pub payload: ToolPayload,
}

#[derive(Clone, Debug)]
pub struct McpToolOutput {
    pub result: CallToolResult,
    pub tool_input: JsonValue,
    pub wall_time: Duration,
    pub original_image_detail_supported: bool,
    pub truncation_policy: TruncationPolicy,
}

impl ToolOutput for McpToolOutput {
    fn log_preview(&self) -> String {
        let payload = self.response_payload();
        let preview = payload.body.to_text().unwrap_or_else(|| {
            serde_json::to_string(&self.result.content)
                .unwrap_or_else(|err| format!("failed to serialize mcp result: {err}"))
        });
        telemetry_preview(&preview)
    }

    fn success_for_logging(&self) -> bool {
        self.result.success()
    }

    fn to_response_item(&self, call_id: &str, _payload: &ToolPayload) -> ResponseInputItem {
        ResponseInputItem::FunctionCallOutput {
            call_id: call_id.to_string(),
            output: self.response_payload(),
        }
    }

    fn code_mode_result(&self, _payload: &ToolPayload) -> JsonValue {
        serde_json::to_value(&self.result).unwrap_or_else(|err| {
            JsonValue::String(format!("failed to serialize mcp result: {err}"))
        })
    }

    fn post_tool_use_input(&self, _payload: &ToolPayload) -> Option<JsonValue> {
        Some(self.tool_input.clone())
    }

    fn post_tool_use_response(&self, _call_id: &str, _payload: &ToolPayload) -> Option<JsonValue> {
        serde_json::to_value(&self.result).ok()
    }
}

impl McpToolOutput {
    fn response_payload(&self) -> FunctionCallOutputPayload {
        let mut payload = self.result.as_function_call_output_payload();
        if let Some(items) = payload.content_items_mut() {
            sanitize_original_image_detail(self.original_image_detail_supported, items);
        }

        let wall_time_seconds = self.wall_time.as_secs_f64();
        let header = format!("Wall time: {wall_time_seconds:.4} seconds\nOutput:");

        match &mut payload.body {
            FunctionCallOutputBody::Text(text) => {
                if text.is_empty() {
                    *text = header;
                } else {
                    *text = format!("{header}\n{text}");
                }
            }
            FunctionCallOutputBody::ContentItems(items) => {
                items.insert(0, FunctionCallOutputContentItem::InputText { text: header });
            }
        }

        // This is the context-injection form, so keep it aligned with the
        // function-call output truncation that conversation history already
        // applies. Code-mode consumers still get the raw `CallToolResult`.
        //
        // The text is serialized again inside the Responses payload, so allow
        // a small buffer for JSON escaping and wrapper overhead.
        truncate_function_output_payload(&payload, self.truncation_policy * 1.2)
    }
}

#[derive(Clone)]
pub struct ToolSearchOutput {
    pub tools: Vec<LoadableToolSpec>,
}

impl ToolOutput for ToolSearchOutput {
    fn log_preview(&self) -> String {
        let tools = self
            .tools
            .iter()
            .map(|tool| {
                serde_json::to_value(tool).unwrap_or_else(|err| {
                    JsonValue::String(format!("failed to serialize tool_search output: {err}"))
                })
            })
            .collect();
        telemetry_preview(&JsonValue::Array(tools).to_string())
    }

    fn success_for_logging(&self) -> bool {
        true
    }

    fn to_response_item(&self, call_id: &str, _payload: &ToolPayload) -> ResponseInputItem {
        ResponseInputItem::ToolSearchOutput {
            call_id: call_id.to_string(),
            status: "completed".to_string(),
            execution: "client".to_string(),
            tools: self
                .tools
                .iter()
                .map(|tool| {
                    serde_json::to_value(tool).unwrap_or_else(|err| {
                        JsonValue::String(format!("failed to serialize tool_search output: {err}"))
                    })
                })
                .collect(),
        }
    }
}

pub struct FunctionToolOutput {
    pub body: Vec<FunctionCallOutputContentItem>,
    pub success: Option<bool>,
    pub post_tool_use_response: Option<JsonValue>,
}

impl FunctionToolOutput {
    pub fn from_text(text: String, success: Option<bool>) -> Self {
        Self {
            body: vec![FunctionCallOutputContentItem::InputText { text }],
            success,
            post_tool_use_response: None,
        }
    }

    pub fn from_content(
        content: Vec<FunctionCallOutputContentItem>,
        success: Option<bool>,
    ) -> Self {
        Self {
            body: content,
            success,
            post_tool_use_response: None,
        }
    }

    pub fn into_text(self) -> String {
        function_call_output_content_items_to_text(&self.body).unwrap_or_default()
    }
}

impl ToolOutput for FunctionToolOutput {
    fn log_preview(&self) -> String {
        telemetry_preview(
            &function_call_output_content_items_to_text(&self.body).unwrap_or_default(),
        )
    }

    fn success_for_logging(&self) -> bool {
        self.success.unwrap_or(true)
    }

    fn exact_duplicate_key(&self) -> Option<Vec<u8>> {
        function_call_output_content_items_to_text(&self.body).map(String::into_bytes)
    }

    fn exact_duplicate_reference(
        &self,
        previous_call_id: &str,
        source_state_sha256: &str,
    ) -> Option<String> {
        serde_json::to_string(&serde_json::json!({
            "same_as_tool_call": previous_call_id,
            "source_state_sha256": source_state_sha256,
        }))
        .ok()
    }

    fn to_response_item(&self, call_id: &str, payload: &ToolPayload) -> ResponseInputItem {
        function_tool_response(call_id, payload, self.body.clone(), self.success)
    }

    fn post_tool_use_response(&self, _call_id: &str, _payload: &ToolPayload) -> Option<JsonValue> {
        self.post_tool_use_response.clone()
    }
}

pub struct ApplyPatchToolOutput {
    pub text: String,
}

impl ApplyPatchToolOutput {
    pub fn from_text(text: String) -> Self {
        Self { text }
    }
}

impl ToolOutput for ApplyPatchToolOutput {
    fn log_preview(&self) -> String {
        telemetry_preview(&self.text)
    }

    fn success_for_logging(&self) -> bool {
        true
    }

    fn to_response_item(&self, call_id: &str, payload: &ToolPayload) -> ResponseInputItem {
        function_tool_response(
            call_id,
            payload,
            vec![FunctionCallOutputContentItem::InputText {
                text: self.text.clone(),
            }],
            Some(true),
        )
    }

    fn post_tool_use_response(&self, _call_id: &str, _payload: &ToolPayload) -> Option<JsonValue> {
        Some(JsonValue::String(self.text.clone()))
    }

    fn code_mode_result(&self, _payload: &ToolPayload) -> JsonValue {
        JsonValue::Object(serde_json::Map::new())
    }
}

pub struct AbortedToolOutput {
    pub message: String,
}

impl ToolOutput for AbortedToolOutput {
    fn log_preview(&self) -> String {
        telemetry_preview(&self.message)
    }

    fn success_for_logging(&self) -> bool {
        false
    }

    fn to_response_item(&self, call_id: &str, payload: &ToolPayload) -> ResponseInputItem {
        match payload {
            ToolPayload::ToolSearch { .. } => ResponseInputItem::ToolSearchOutput {
                call_id: call_id.to_string(),
                status: "completed".to_string(),
                execution: "client".to_string(),
                tools: Vec::new(),
            },
            _ => function_tool_response(
                call_id,
                payload,
                vec![FunctionCallOutputContentItem::InputText {
                    text: self.message.clone(),
                }],
                /*success*/ None,
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExecCommandToolOutput {
    pub event_call_id: String,
    pub chunk_id: String,
    pub wall_time: Duration,
    /// Raw bytes returned for this unified exec call before any truncation.
    pub raw_output: Vec<u8>,
    pub truncation_policy: TruncationPolicy,
    pub max_output_tokens: Option<usize>,
    pub process_id: Option<i32>,
    pub exit_code: Option<i32>,
    pub original_token_count: Option<usize>,
    /// Bytes omitted by the output collection cap before model-facing truncation.
    pub output_omitted_bytes: Option<NonZeroUsize>,
    pub hook_command: Option<String>,
    pub compact_model_payload: bool,
    pub lossless_terminal_codec: bool,
    pub command_aware_projection: bool,
}

impl ToolOutput for ExecCommandToolOutput {
    fn log_preview(&self) -> String {
        telemetry_preview(&self.response_text())
    }

    fn success_for_logging(&self) -> bool {
        true
    }

    fn exact_duplicate_key(&self) -> Option<Vec<u8>> {
        Some(self.raw_output.clone())
    }

    fn exact_duplicate_reference(
        &self,
        previous_call_id: &str,
        source_state_sha256: &str,
    ) -> Option<String> {
        #[derive(Serialize)]
        struct DuplicateExecOutput<'a> {
            #[serde(skip_serializing_if = "str::is_empty")]
            chunk_id: &'a str,
            wall_time_seconds: f64,
            #[serde(skip_serializing_if = "Option::is_none")]
            exit_code: Option<i32>,
            #[serde(skip_serializing_if = "Option::is_none")]
            process_id: Option<i32>,
            #[serde(skip_serializing_if = "Option::is_none")]
            original_token_count: Option<usize>,
            #[serde(skip_serializing_if = "Option::is_none")]
            output_omitted_bytes: Option<usize>,
            raw_sha256: String,
            raw_byte_count: usize,
            same_output_as_tool_call: &'a str,
            source_state_sha256: &'a str,
        }

        serde_json::to_string(&DuplicateExecOutput {
            chunk_id: &self.chunk_id,
            wall_time_seconds: self.wall_time.as_secs_f64(),
            exit_code: self.exit_code,
            process_id: self.process_id,
            original_token_count: self.original_token_count,
            output_omitted_bytes: self.output_omitted_bytes.map(NonZeroUsize::get),
            raw_sha256: codex_zero_codec::sha256(&self.raw_output),
            raw_byte_count: self.raw_output.len(),
            same_output_as_tool_call: previous_call_id,
            source_state_sha256,
        })
        .ok()
    }

    fn to_response_item(&self, call_id: &str, payload: &ToolPayload) -> ResponseInputItem {
        function_tool_response(
            call_id,
            payload,
            vec![FunctionCallOutputContentItem::InputText {
                text: self.model_response_text(),
            }],
            Some(true),
        )
    }

    fn post_tool_use_id(&self, call_id: &str) -> String {
        if self.event_call_id.is_empty() {
            call_id.to_string()
        } else {
            self.event_call_id.clone()
        }
    }

    fn post_tool_use_input(&self, _payload: &ToolPayload) -> Option<JsonValue> {
        self.hook_command
            .as_ref()
            .map(|command| serde_json::json!({ "command": command }))
    }

    fn post_tool_use_response(&self, _call_id: &str, _payload: &ToolPayload) -> Option<JsonValue> {
        if self.process_id.is_some() || self.hook_command.is_none() {
            return None;
        }

        Some(JsonValue::String(
            self.truncated_output(self.model_output_max_tokens()),
        ))
    }

    fn code_mode_result(&self, _payload: &ToolPayload) -> JsonValue {
        #[derive(Serialize)]
        struct UnifiedExecCodeModeResult {
            #[serde(skip_serializing_if = "Option::is_none")]
            chunk_id: Option<String>,
            wall_time_seconds: f64,
            #[serde(skip_serializing_if = "Option::is_none")]
            exit_code: Option<i32>,
            #[serde(skip_serializing_if = "Option::is_none")]
            session_id: Option<i32>,
            #[serde(skip_serializing_if = "Option::is_none")]
            original_token_count: Option<usize>,
            output: String,
        }

        let result = UnifiedExecCodeModeResult {
            chunk_id: (!self.chunk_id.is_empty()).then(|| self.chunk_id.clone()),
            wall_time_seconds: self.wall_time.as_secs_f64(),
            exit_code: self.exit_code,
            session_id: self.process_id,
            original_token_count: self.original_token_count,
            output: match self.max_output_tokens {
                Some(max_tokens) => self.truncated_output(max_tokens),
                None => String::from_utf8_lossy(&self.raw_output).to_string(),
            },
        };

        serde_json::to_value(result).unwrap_or_else(|err| {
            JsonValue::String(format!("failed to serialize exec result: {err}"))
        })
    }
}

impl ExecCommandToolOutput {
    fn model_output_max_tokens(&self) -> usize {
        resolve_max_tokens(self.max_output_tokens).min(self.truncation_policy.token_budget())
    }

    pub(crate) fn truncated_output(&self, max_tokens: usize) -> String {
        let text = String::from_utf8_lossy(&self.raw_output).to_string();
        let policy = TruncationPolicy::Tokens(max_tokens);
        let Some(omitted_bytes) = self.output_omitted_bytes else {
            return formatted_truncate_text(&text, policy);
        };

        let marker = format_output_omission_marker(omitted_bytes.get());
        if text.len() <= policy.byte_budget() {
            return if text.contains(&marker) {
                text
            } else {
                format!("{marker}\n{text}")
            };
        }

        let original_token_count = self
            .original_token_count
            .unwrap_or_else(|| approx_token_count(&text));
        let truncated = truncate_text(&text, policy);
        let omission_notice = if truncated.contains(&marker) {
            String::new()
        } else {
            format!("{marker}\n")
        };
        format!(
            "Warning: truncated output (original token count: {original_token_count})\n{omission_notice}\n{truncated}"
        )
    }

    fn response_text(&self) -> String {
        let mut sections = Vec::new();

        if !self.chunk_id.is_empty() {
            sections.push(format!("Chunk ID: {}", self.chunk_id));
        }

        let wall_time_seconds = self.wall_time.as_secs_f64();
        sections.push(format!("Wall time: {wall_time_seconds:.4} seconds"));

        if let Some(exit_code) = self.exit_code {
            sections.push(format!("Process exited with code {exit_code}"));
        }

        if let Some(process_id) = &self.process_id {
            sections.push(format!("Process running with session ID {process_id}"));
        }

        if let Some(original_token_count) = self.original_token_count {
            sections.push(format!("Original token count: {original_token_count}"));
        }

        sections.push("Output:".to_string());
        sections.push(self.truncated_output(self.model_output_max_tokens()));

        sections.join("\n")
    }

    fn model_response_text(&self) -> String {
        let original = self.response_text();
        if !self.compact_model_payload
            && !self.lossless_terminal_codec
            && !self.command_aware_projection
        {
            return original;
        }

        let artifact_root = std::env::var_os("CODEX_ZERO_ARTIFACT_DIR")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                find_codex_home()
                    .ok()
                    .map(|codex_home| codex_zero_default_artifact_root(codex_home.as_path()))
            });
        let Some(artifact_root) = artifact_root else {
            return original;
        };
        let telemetry_path = std::env::var_os("CODEX_ZERO_TELEMETRY_FILE")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from);
        self.model_response_text_with_paths(original, &artifact_root, telemetry_path.as_deref())
    }

    fn model_response_text_with_paths(
        &self,
        original: String,
        artifact_root: &std::path::Path,
        telemetry_path: Option<&std::path::Path>,
    ) -> String {
        let artifact = match store_raw(artifact_root, &self.raw_output) {
            Ok(artifact) => artifact,
            Err(error) => {
                tracing::warn!(
                    %error,
                    artifact_root = %artifact_root.display(),
                    "failed to preserve raw CodexZero output; returning stock output"
                );
                return original;
            }
        };

        let truncated = self.truncated_output(self.model_output_max_tokens());
        let terminal = if self.command_aware_projection {
            encode_command_output(
                truncated.as_bytes(),
                self.hook_command.as_deref(),
                self.exit_code,
            )
        } else if self.lossless_terminal_codec {
            codex_zero_codec::encode_terminal(truncated.as_bytes())
        } else {
            codex_zero_codec::TerminalEncoding {
                text: truncated,
                codec: None,
                projection: None,
                original_line_count: None,
                presentation_changed: false,
            }
        };

        #[derive(Serialize)]
        struct ArtifactMetadata<'a> {
            sha256: &'a str,
            raw_byte_count: usize,
            #[serde(skip_serializing_if = "Option::is_none")]
            original_token_count: Option<usize>,
        }

        #[derive(Serialize)]
        struct CompactExecOutput<'a> {
            #[serde(skip_serializing_if = "str::is_empty")]
            chunk_id: &'a str,
            wall_time_seconds: f64,
            #[serde(skip_serializing_if = "Option::is_none")]
            exit_code: Option<i32>,
            #[serde(skip_serializing_if = "Option::is_none")]
            process_id: Option<i32>,
            #[serde(skip_serializing_if = "Option::is_none")]
            original_token_count: Option<usize>,
            #[serde(skip_serializing_if = "Option::is_none")]
            output_omitted_bytes: Option<usize>,
            artifact: ArtifactMetadata<'a>,
            #[serde(skip_serializing_if = "Option::is_none")]
            codec: Option<&'static str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            projection: Option<&'static str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            original_line_count: Option<usize>,
            #[serde(skip_serializing_if = "is_false")]
            presentation_styling_removed: bool,
            #[serde(skip_serializing_if = "str::is_empty")]
            output: &'a str,
        }

        let candidate = CompactExecOutput {
            chunk_id: &self.chunk_id,
            wall_time_seconds: self.wall_time.as_secs_f64(),
            exit_code: self.exit_code,
            process_id: self.process_id,
            original_token_count: self.original_token_count,
            output_omitted_bytes: self.output_omitted_bytes.map(NonZeroUsize::get),
            artifact: ArtifactMetadata {
                sha256: &artifact.sha256,
                raw_byte_count: artifact.byte_count,
                original_token_count: self.original_token_count,
            },
            codec: terminal.codec,
            projection: terminal.projection,
            original_line_count: terminal.original_line_count,
            presentation_styling_removed: terminal.presentation_changed,
            output: &terminal.text,
        };
        let Ok(candidate) = serde_json::to_string(&candidate) else {
            return original;
        };
        let selection = select_strictly_smaller(&original, candidate);
        self.record_codex_zero_selection(
            telemetry_path,
            &artifact.sha256,
            terminal.codec,
            terminal.projection,
            &selection,
        );
        selection.text
    }

    fn record_codex_zero_selection(
        &self,
        telemetry_path: Option<&std::path::Path>,
        artifact_sha256: &str,
        codec: Option<&str>,
        projection: Option<&str>,
        selection: &codex_zero_codec::MonotonicSelection,
    ) {
        let Some(path) = telemetry_path else {
            return;
        };
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or_default();
        let record = serde_json::json!({
            "schema": "codex-zero-telemetry-v1",
            "event": "exec_model_payload",
            "timestamp_ms": timestamp_ms,
            "artifact_sha256": artifact_sha256,
            "raw_byte_count": self.raw_output.len(),
            "original_tokens": selection.original_tokens,
            "selected_tokens": selection.selected_tokens,
            "tokens_eliminated": selection.original_tokens.saturating_sub(selection.selected_tokens),
            "transformed": selection.transformed,
            "codec": codec,
            "projection": projection,
        });
        let _ = append_telemetry_to_path(path, &record);
    }
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(value: &bool) -> bool {
    !*value
}

fn codex_zero_default_artifact_root(codex_home: &Path) -> PathBuf {
    codex_home.join("codexzero").join("artifacts")
}

fn function_tool_response(
    call_id: &str,
    payload: &ToolPayload,
    body: Vec<FunctionCallOutputContentItem>,
    success: Option<bool>,
) -> ResponseInputItem {
    let body = match body.as_slice() {
        [FunctionCallOutputContentItem::InputText { text }] => {
            FunctionCallOutputBody::Text(text.clone())
        }
        _ => FunctionCallOutputBody::ContentItems(body),
    };

    if matches!(payload, ToolPayload::Custom { .. }) {
        return ResponseInputItem::CustomToolCallOutput {
            call_id: call_id.to_string(),
            name: None,
            output: FunctionCallOutputPayload { body, success },
        };
    }

    ResponseInputItem::FunctionCallOutput {
        call_id: call_id.to_string(),
        output: FunctionCallOutputPayload { body, success },
    }
}

fn telemetry_preview(content: &str) -> String {
    let truncated_slice = take_bytes_at_char_boundary(content, TELEMETRY_PREVIEW_MAX_BYTES);
    let truncated_by_bytes = truncated_slice.len() < content.len();

    let mut preview = String::new();
    let mut lines_iter = truncated_slice.lines();
    for idx in 0..TELEMETRY_PREVIEW_MAX_LINES {
        match lines_iter.next() {
            Some(line) => {
                if idx > 0 {
                    preview.push('\n');
                }
                preview.push_str(line);
            }
            None => break,
        }
    }
    let truncated_by_lines = lines_iter.next().is_some();

    if !truncated_by_bytes && !truncated_by_lines {
        return content.to_string();
    }

    if preview.len() < truncated_slice.len()
        && truncated_slice
            .as_bytes()
            .get(preview.len())
            .is_some_and(|byte| *byte == b'\n')
    {
        preview.push('\n');
    }

    if !preview.is_empty() && !preview.ends_with('\n') {
        preview.push('\n');
    }
    preview.push_str(TELEMETRY_PREVIEW_TRUNCATION_NOTICE);

    preview
}

#[cfg(test)]
#[path = "context_tests.rs"]
mod tests;
