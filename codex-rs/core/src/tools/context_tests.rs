use super::*;
use codex_protocol::models::DEFAULT_IMAGE_DETAIL;
use codex_protocol::models::SearchToolCallParams;
use core_test_support::assert_regex_match;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn custom_tool_calls_should_roundtrip_as_custom_outputs() {
    let payload = ToolPayload::Custom {
        input: "patch".to_string(),
    };
    let response = FunctionToolOutput::from_text("patched".to_string(), Some(true))
        .to_response_item("call-42", &payload);

    match response {
        ResponseInputItem::CustomToolCallOutput {
            call_id, output, ..
        } => {
            assert_eq!(call_id, "call-42");
            assert_eq!(output.content_items(), None);
            assert_eq!(output.body.to_text().as_deref(), Some("patched"));
            assert_eq!(output.success, Some(true));
        }
        other => panic!("expected CustomToolCallOutput, got {other:?}"),
    }
}

#[test]
fn function_payloads_remain_function_outputs() {
    let payload = ToolPayload::Function {
        arguments: "{}".to_string(),
    };
    let response = FunctionToolOutput::from_text("ok".to_string(), Some(true))
        .to_response_item("fn-1", &payload);

    match response {
        ResponseInputItem::FunctionCallOutput { call_id, output } => {
            assert_eq!(call_id, "fn-1");
            assert_eq!(output.content_items(), None);
            assert_eq!(output.body.to_text().as_deref(), Some("ok"));
            assert_eq!(output.success, Some(true));
        }
        other => panic!("expected FunctionCallOutput, got {other:?}"),
    }
}

#[test]
fn mcp_code_mode_result_serializes_full_call_tool_result() {
    let output = CallToolResult {
        content: vec![serde_json::json!({
            "type": "text",
            "text": "ignored",
        })],
        structured_content: Some(serde_json::json!({
            "threadId": "thread_123",
            "content": "done",
        })),
        is_error: Some(false),
        meta: Some(serde_json::json!({
            "source": "mcp",
        })),
    };

    let result = output.code_mode_result(&ToolPayload::Function {
        arguments: "{}".to_string(),
    });

    assert_eq!(
        result,
        serde_json::json!({
            "content": [{
                "type": "text",
                "text": "ignored",
            }],
            "structuredContent": {
                "threadId": "thread_123",
                "content": "done",
            },
            "isError": false,
            "_meta": {
                "source": "mcp",
            },
        })
    );
}

#[test]
fn mcp_tool_output_response_item_includes_wall_time() {
    let output = McpToolOutput {
        result: CallToolResult {
            content: vec![serde_json::json!({
                "type": "text",
                "text": "done",
            })],
            structured_content: None,
            is_error: Some(false),
            meta: None,
        },
        tool_input: json!({}),
        wall_time: std::time::Duration::from_millis(1250),
        original_image_detail_supported: false,
        truncation_policy: TruncationPolicy::Bytes(1024),
    };

    let response = output.to_response_item(
        "mcp-call-1",
        &ToolPayload::Function {
            arguments: "{}".to_string(),
        },
    );

    match response {
        ResponseInputItem::FunctionCallOutput { call_id, output } => {
            assert_eq!(call_id, "mcp-call-1");
            assert_eq!(output.success, Some(true));
            let Some(text) = output.body.to_text() else {
                panic!("MCP output should serialize as text");
            };
            let Some(payload) = text.strip_prefix("Wall time: 1.2500 seconds\nOutput:\n") else {
                panic!("MCP output should include wall-time header: {text}");
            };
            let parsed: serde_json::Value = serde_json::from_str(payload).unwrap_or_else(|err| {
                panic!("MCP output should serialize JSON content: {err}");
            });
            assert_eq!(
                parsed,
                json!([{
                    "type": "text",
                    "text": "done",
                }])
            );
        }
        other => panic!("expected FunctionCallOutput, got {other:?}"),
    }
}

#[test]
fn mcp_tool_output_response_item_truncates_large_structured_content() {
    let output = McpToolOutput {
        result: CallToolResult {
            content: vec![serde_json::json!({
                "type": "text",
                "text": "ignored when structured content is present",
            })],
            structured_content: Some(serde_json::json!({
                "items": "large structured value ".repeat(1_000),
            })),
            is_error: Some(false),
            meta: None,
        },
        tool_input: json!({}),
        wall_time: std::time::Duration::from_millis(1250),
        original_image_detail_supported: false,
        truncation_policy: TruncationPolicy::Bytes(128),
    };

    let response = output.to_response_item(
        "mcp-call-large",
        &ToolPayload::Function {
            arguments: "{}".to_string(),
        },
    );

    match response {
        ResponseInputItem::FunctionCallOutput { call_id, output } => {
            assert_eq!(call_id, "mcp-call-large");
            assert_eq!(output.success, Some(true));
            let text = output
                .body
                .to_text()
                .expect("MCP output should serialize as text");
            assert!(text.starts_with("Wall time: 1.2500 seconds\nOutput:\n"));
            assert!(text.contains("chars truncated"));
            assert!(!text.contains("ignored when structured content is present"));
        }
        other => panic!("expected FunctionCallOutput, got {other:?}"),
    }
}

#[test]
fn mcp_tool_output_response_item_preserves_content_items() {
    let image_url = "data:image/png;base64,AAA";
    let output = McpToolOutput {
        result: CallToolResult {
            content: vec![serde_json::json!({
                "type": "image",
                "mimeType": "image/png",
                "data": "AAA",
            })],
            structured_content: None,
            is_error: Some(false),
            meta: None,
        },
        tool_input: json!({}),
        wall_time: std::time::Duration::from_millis(500),
        original_image_detail_supported: false,
        truncation_policy: TruncationPolicy::Bytes(1024),
    };

    let response = output.to_response_item(
        "mcp-call-2",
        &ToolPayload::Function {
            arguments: "{}".to_string(),
        },
    );

    match response {
        ResponseInputItem::FunctionCallOutput { output, .. } => {
            assert_eq!(
                output.content_items(),
                Some(
                    vec![
                        FunctionCallOutputContentItem::InputText {
                            text: "Wall time: 0.5000 seconds\nOutput:".to_string(),
                        },
                        FunctionCallOutputContentItem::InputImage {
                            image_url: image_url.to_string(),
                            detail: Some(DEFAULT_IMAGE_DETAIL),
                        },
                    ]
                    .as_slice()
                )
            );
            assert_eq!(
                output.body.to_text().as_deref(),
                Some("Wall time: 0.5000 seconds\nOutput:")
            );
        }
        other => panic!("expected FunctionCallOutput, got {other:?}"),
    }
}

#[test]
fn mcp_tool_output_code_mode_result_stays_raw_call_tool_result() {
    let large_content = "large structured value ".repeat(1_000);
    let output = McpToolOutput {
        result: CallToolResult {
            content: vec![serde_json::json!({
                "type": "text",
                "text": "ignored",
            })],
            structured_content: Some(serde_json::json!({
                "content": large_content,
            })),
            is_error: Some(false),
            meta: None,
        },
        tool_input: json!({}),
        wall_time: std::time::Duration::from_millis(1250),
        original_image_detail_supported: false,
        truncation_policy: TruncationPolicy::Bytes(64),
    };

    let result = output.code_mode_result(&ToolPayload::Function {
        arguments: "{}".to_string(),
    });

    assert_eq!(
        result,
        serde_json::json!({
            "content": [{
                "type": "text",
                "text": "ignored",
            }],
            "structuredContent": {
                "content": "large structured value ".repeat(1_000),
            },
            "isError": false,
        })
    );
}

#[test]
fn custom_tool_calls_can_derive_text_from_content_items() {
    let payload = ToolPayload::Custom {
        input: "patch".to_string(),
    };
    let response = FunctionToolOutput::from_content(
        vec![
            FunctionCallOutputContentItem::InputText {
                text: "line 1".to_string(),
            },
            FunctionCallOutputContentItem::InputImage {
                image_url: "data:image/png;base64,AAA".to_string(),
                detail: Some(DEFAULT_IMAGE_DETAIL),
            },
            FunctionCallOutputContentItem::InputText {
                text: "line 2".to_string(),
            },
        ],
        Some(true),
    )
    .to_response_item("call-99", &payload);

    match response {
        ResponseInputItem::CustomToolCallOutput {
            call_id, output, ..
        } => {
            let expected = vec![
                FunctionCallOutputContentItem::InputText {
                    text: "line 1".to_string(),
                },
                FunctionCallOutputContentItem::InputImage {
                    image_url: "data:image/png;base64,AAA".to_string(),
                    detail: Some(DEFAULT_IMAGE_DETAIL),
                },
                FunctionCallOutputContentItem::InputText {
                    text: "line 2".to_string(),
                },
            ];
            assert_eq!(call_id, "call-99");
            assert_eq!(output.content_items(), Some(expected.as_slice()));
            assert_eq!(output.body.to_text().as_deref(), Some("line 1\nline 2"));
            assert_eq!(output.success, Some(true));
        }
        other => panic!("expected CustomToolCallOutput, got {other:?}"),
    }
}

#[test]
fn tool_search_payloads_roundtrip_as_tool_search_outputs() {
    let payload = ToolPayload::ToolSearch {
        arguments: SearchToolCallParams {
            query: "calendar".to_string(),
            limit: None,
        },
    };
    let response = ToolSearchOutput {
        tools: vec![LoadableToolSpec::Function(codex_tools::ResponsesApiTool {
            name: "create_event".to_string(),
            description: String::new(),
            strict: false,
            defer_loading: Some(true),
            parameters: codex_tools::JsonSchema::object(
                /*properties*/ Default::default(),
                /*required*/ None,
                /*additional_properties*/ None,
            ),
            output_schema: None,
        })],
    }
    .to_response_item("search-1", &payload);

    match response {
        ResponseInputItem::ToolSearchOutput {
            call_id,
            status,
            execution,
            tools,
        } => {
            assert_eq!(call_id, "search-1");
            assert_eq!(status, "completed");
            assert_eq!(execution, "client");
            assert_eq!(
                tools,
                vec![json!({
                    "type": "function",
                    "name": "create_event",
                    "description": "",
                    "strict": false,
                    "defer_loading": true,
                    "parameters": {
                        "type": "object",
                        "properties": {}
                    }
                })]
            );
        }
        other => panic!("expected ToolSearchOutput, got {other:?}"),
    }
}

#[test]
fn log_preview_uses_content_items_when_plain_text_is_missing() {
    let output = FunctionToolOutput::from_content(
        vec![FunctionCallOutputContentItem::InputText {
            text: "preview".to_string(),
        }],
        Some(true),
    );

    assert_eq!(output.log_preview(), "preview");
    assert_eq!(
        function_call_output_content_items_to_text(&output.body),
        Some("preview".to_string())
    );
}

#[test]
fn telemetry_preview_returns_original_within_limits() {
    let content = "short output";
    assert_eq!(telemetry_preview(content), content);
}

#[test]
fn telemetry_preview_truncates_by_bytes() {
    let content = "x".repeat(TELEMETRY_PREVIEW_MAX_BYTES + 8);
    let preview = telemetry_preview(&content);

    assert!(preview.contains(TELEMETRY_PREVIEW_TRUNCATION_NOTICE));
    assert!(
        preview.len()
            <= TELEMETRY_PREVIEW_MAX_BYTES + TELEMETRY_PREVIEW_TRUNCATION_NOTICE.len() + 1
    );
}

#[test]
fn telemetry_preview_truncates_by_lines() {
    let content = (0..(TELEMETRY_PREVIEW_MAX_LINES + 5))
        .map(|idx| format!("line {idx}"))
        .collect::<Vec<_>>()
        .join("\n");

    let preview = telemetry_preview(&content);
    let lines: Vec<&str> = preview.lines().collect();

    assert!(lines.len() <= TELEMETRY_PREVIEW_MAX_LINES + 1);
    assert_eq!(lines.last(), Some(&TELEMETRY_PREVIEW_TRUNCATION_NOTICE));
}

#[test]
fn exec_command_tool_output_formats_truncated_response() {
    let payload = ToolPayload::Function {
        arguments: "{}".to_string(),
    };
    let response = ExecCommandToolOutput {
        event_call_id: "call-42".to_string(),
        chunk_id: "abc123".to_string(),
        wall_time: std::time::Duration::from_millis(1250),
        raw_output: b"token one token two token three token four token five".to_vec(),
        truncation_policy: TruncationPolicy::Tokens(10_000),
        max_output_tokens: Some(4),
        process_id: None,
        exit_code: Some(0),
        original_token_count: Some(10),
        output_omitted_bytes: None,
        hook_command: None,
        compact_model_payload: false,
        lossless_terminal_codec: false,
        command_aware_projection: false,
    }
    .to_response_item("call-42", &payload);

    match response {
        ResponseInputItem::FunctionCallOutput { call_id, output } => {
            assert_eq!(call_id, "call-42");
            assert_eq!(output.success, Some(true));
            let text = output
                .body
                .to_text()
                .expect("exec output should serialize as text");
            assert_regex_match(
                r#"(?sx)
                    ^Chunk\ ID:\ abc123
                    \nWall\ time:\ \d+\.\d{4}\ seconds
                    \nProcess\ exited\ with\ code\ 0
                    \nOriginal\ token\ count:\ 10
                    \nOutput:
                    \n.*tokens\ truncated.*
                    $"#,
                &text,
            );
        }
        other => panic!("expected FunctionCallOutput, got {other:?}"),
    }
}

#[test]
fn exec_command_tool_output_preserves_omission_metadata_when_truncated() {
    let payload = ToolPayload::Function {
        arguments: "{}".to_string(),
    };
    let marker = format_output_omission_marker(/*omitted_bytes*/ 123_456);
    let raw_output = format!(
        "HEAD-{}\n{marker}\nTAIL-{}",
        "a".repeat(/*n*/ 100),
        "z".repeat(/*n*/ 100)
    )
    .into_bytes();
    let response = ExecCommandToolOutput {
        event_call_id: "call-omitted".to_string(),
        chunk_id: "abc123".to_string(),
        wall_time: std::time::Duration::from_millis(/*millis*/ 1250),
        raw_output,
        truncation_policy: TruncationPolicy::Tokens(10_000),
        max_output_tokens: Some(4),
        process_id: None,
        exit_code: Some(0),
        original_token_count: Some(42_000),
        output_omitted_bytes: NonZeroUsize::new(/*n*/ 123_456),
        hook_command: None,
        compact_model_payload: false,
        lossless_terminal_codec: false,
        command_aware_projection: false,
    }
    .to_response_item("call-omitted", &payload);

    let ResponseInputItem::FunctionCallOutput { output, .. } = response else {
        panic!("expected FunctionCallOutput");
    };
    let text = output
        .body
        .to_text()
        .expect("exec output should serialize as text");
    assert!(text.contains("Original token count: 42000"));
    assert!(text.contains("Warning: truncated output (original token count: 42000)"));
    assert_eq!(text.matches(&marker).count(), 1);
}

#[test]
fn codex_zero_exec_payload_is_selected_only_when_strictly_smaller() {
    let temp = tempfile::tempdir().expect("temporary artifact directory");
    let telemetry_path = temp.path().join("telemetry.jsonl");
    let raw_output = "same diagnostic line\n".repeat(400).into_bytes();
    let output = ExecCommandToolOutput {
        event_call_id: "call-zero".to_string(),
        chunk_id: "zero123".to_string(),
        wall_time: std::time::Duration::from_millis(1250),
        raw_output: raw_output.clone(),
        truncation_policy: TruncationPolicy::Tokens(100_000),
        max_output_tokens: None,
        process_id: None,
        exit_code: Some(0),
        original_token_count: Some(1600),
        output_omitted_bytes: None,
        hook_command: Some("ignored model command".to_string()),
        compact_model_payload: true,
        lossless_terminal_codec: true,
        command_aware_projection: true,
    };

    let original = output.response_text();
    let selected =
        output.model_response_text_with_paths(original.clone(), temp.path(), Some(&telemetry_path));

    assert!(codex_zero_codec::token_count(&selected) < codex_zero_codec::token_count(&original));
    assert!(!selected.contains("Output:"));
    assert!(!selected.contains("ignored model command"));
    let payload: serde_json::Value =
        serde_json::from_str(&selected).expect("compact payload should be JSON");
    let sha = payload["artifact"]["sha256"]
        .as_str()
        .expect("artifact hash");
    assert_eq!(
        std::fs::read(temp.path().join("sha256").join(sha)).expect("raw artifact"),
        raw_output
    );
    let telemetry = std::fs::read_to_string(telemetry_path).expect("telemetry record");
    assert!(telemetry.contains(r#""transformed":true"#));
}

#[test]
fn codex_zero_exec_payload_falls_back_when_candidate_is_not_smaller() {
    let temp = tempfile::tempdir().expect("temporary artifact directory");
    let output = ExecCommandToolOutput {
        event_call_id: "call-zero-small".to_string(),
        chunk_id: String::new(),
        wall_time: std::time::Duration::ZERO,
        raw_output: Vec::new(),
        truncation_policy: TruncationPolicy::Tokens(100),
        max_output_tokens: None,
        process_id: None,
        exit_code: None,
        original_token_count: None,
        output_omitted_bytes: None,
        hook_command: None,
        compact_model_payload: true,
        lossless_terminal_codec: true,
        command_aware_projection: true,
    };

    let original = output.response_text();
    assert_eq!(
        output.model_response_text_with_paths(original.clone(), temp.path(), None),
        original
    );
}

#[test]
fn codex_zero_exact_duplicate_key_uses_full_raw_output() {
    let prefix = "same prefix\n".repeat(200);
    let suffix = "same suffix\n".repeat(200);
    let make_output = |middle: &str| ExecCommandToolOutput {
        event_call_id: "call-exact".to_string(),
        chunk_id: "exact123".to_string(),
        wall_time: std::time::Duration::ZERO,
        raw_output: format!("{prefix}{middle}{suffix}").into_bytes(),
        truncation_policy: TruncationPolicy::Tokens(10_000),
        max_output_tokens: Some(32),
        process_id: None,
        exit_code: Some(0),
        original_token_count: None,
        output_omitted_bytes: None,
        hook_command: None,
        compact_model_payload: false,
        lossless_terminal_codec: false,
        command_aware_projection: false,
    };
    let first = make_output("hidden value 1\n");
    let second = make_output("hidden value 2\n");

    assert_eq!(
        first.truncated_output(first.model_output_max_tokens()),
        second.truncated_output(second.model_output_max_tokens())
    );
    assert_ne!(first.exact_duplicate_key(), second.exact_duplicate_key());
}

#[test]
fn codex_zero_successful_check_projection_is_artifact_backed_and_recorded() {
    let temp = tempfile::tempdir().expect("temporary artifact directory");
    let telemetry_path = temp.path().join("telemetry.jsonl");
    let mut raw_output = String::from("test suite\nplatform windows\nstarting\n");
    for index in 0..140 {
        raw_output.push_str(&format!(
            "completed fixture {index} with value {}\n",
            index * 17
        ));
    }
    raw_output.push_str("warning: deprecated fixture\nsrc/check.js:10\n");
    raw_output.push_str("Tests: 140 passed, 140 total\n");
    let raw_output = raw_output.into_bytes();
    let output = ExecCommandToolOutput {
        event_call_id: "call-projection".to_string(),
        chunk_id: "projection123".to_string(),
        wall_time: std::time::Duration::from_millis(1250),
        raw_output: raw_output.clone(),
        truncation_policy: TruncationPolicy::Tokens(100_000),
        max_output_tokens: None,
        process_id: None,
        exit_code: Some(0),
        original_token_count: Some(2000),
        output_omitted_bytes: None,
        hook_command: Some("npm test".to_string()),
        compact_model_payload: true,
        lossless_terminal_codec: true,
        command_aware_projection: true,
    };

    let selected = output.model_response_text_with_paths(
        output.response_text(),
        temp.path(),
        Some(&telemetry_path),
    );
    let payload: serde_json::Value =
        serde_json::from_str(&selected).expect("compact payload should be JSON");
    assert_eq!(payload["projection"], "successful-check-v1");
    assert_eq!(payload["original_line_count"], 146);
    assert!(payload["output"].as_str().is_some_and(|text| {
        text.contains("warning: deprecated fixture")
            && text.contains("Tests: 140 passed, 140 total")
    }));
    let sha = payload["artifact"]["sha256"]
        .as_str()
        .expect("artifact hash");
    assert_eq!(
        std::fs::read(temp.path().join("sha256").join(sha)).expect("raw artifact"),
        raw_output
    );
    let telemetry = std::fs::read_to_string(telemetry_path).expect("telemetry record");
    assert!(telemetry.contains(r#""projection":"successful-check-v1""#));
}
