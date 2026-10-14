use super::*;
use pretty_assertions::assert_eq;

#[test]
fn strips_styling_and_preserves_hyperlink_target() {
    let raw = b"\x1b[1;34mblue\x1b[0m \x1b]8;;https://example.com\x07docs\x1b]8;;\x07\n";
    let encoded = encode_terminal(raw);

    assert_eq!(
        decode_terminal(&encoded).unwrap(),
        "blue docs (https://example.com)\n"
    );
    assert!(encoded.presentation_changed);
}

#[test]
fn preserves_non_styling_terminal_control_sequences() {
    let raw = b"\x1b[2Jkept\x1b]0;title\x07\n";
    let encoded = encode_terminal(raw);
    assert_eq!(
        decode_terminal(&encoded).unwrap(),
        "\x1b[2Jkept\x1b]0;title\x07\n"
    );
}

#[test]
fn repeated_complete_lines_round_trip_with_exact_count() {
    let raw = "same diagnostic\n".repeat(250);
    let encoded = encode_terminal(raw.as_bytes());

    assert_eq!(encoded.codec, Some(RLE_CODEC));
    assert_eq!(decode_terminal(&encoded).unwrap(), raw);
    assert!(token_count(&encoded.text) < token_count(&raw));
}

#[test]
fn non_identical_lines_are_not_collapsed() {
    let raw = "first\nsecond\nthird\n";
    let encoded = encode_terminal(raw.as_bytes());

    assert_eq!(decode_terminal(&encoded).unwrap(), raw);
}

#[test]
fn successful_checks_keep_diagnostics_and_tail_in_a_smaller_projection() {
    let mut output = String::from("test suite\nplatform windows\nstarting\n");
    output.push_str(&"heartbeat\n".repeat(100));
    output.push_str("warning: deprecated fixture\nsrc/check.js:10\n");
    output.push_str(&"heartbeat\n".repeat(40));
    output.push_str("Tests: 12 passed, 12 total\n");

    let encoded = encode_command_output(output.as_bytes(), Some("npm test"), Some(0));

    assert_eq!(encoded.projection, Some(SUCCESSFUL_CHECK_PROJECTION));
    assert_eq!(encoded.original_line_count, Some(146));
    assert!(encoded.text.contains("warning: deprecated fixture"));
    assert!(encoded.text.contains("src/check.js:10"));
    assert!(encoded.text.contains("Tests: 12 passed, 12 total"));
    assert!(token_count(&encoded.text) < token_count(&output));
    assert!(decode_terminal(&encoded).is_err());
}

#[test]
fn failed_or_unknown_commands_are_never_projected() {
    let output = "failure detail\n".repeat(100);
    for encoded in [
        encode_command_output(output.as_bytes(), Some("npm test"), Some(1)),
        encode_command_output(output.as_bytes(), Some("cat test.log"), Some(0)),
    ] {
        assert_eq!(encoded.projection, None);
        assert_eq!(decode_terminal(&encoded).unwrap(), output);
    }
}

#[test]
fn raw_artifact_hash_and_bytes_are_exact() {
    let directory = tempfile::tempdir().unwrap();
    let raw = b"\x00raw\r\n\xffbytes";
    let artifact = store_raw(directory.path(), raw).unwrap();

    assert_eq!(
        artifact.sha256,
        "af9510137a462abb539c532d5281d8d1a1373a6445f639e5876ba95e65e23c87"
    );
    assert_eq!(artifact.byte_count, raw.len());
    assert_eq!(std::fs::read(artifact.path).unwrap(), raw);
}

#[test]
fn raw_artifact_store_rejects_corrupt_existing_content() {
    let temp = tempfile::tempdir().unwrap();
    let raw = b"trusted bytes";
    let artifact = store_raw(temp.path(), raw).unwrap();
    std::fs::write(&artifact.path, b"corrupt").unwrap();
    let error = store_raw(temp.path(), raw).expect_err("corrupt artifact must fail closed");
    assert!(error.to_string().contains("collision or corruption"));
}

#[test]
fn equal_or_larger_candidate_falls_back_to_original() {
    let original = "short";
    let selected = select_strictly_smaller(original, "not any shorter".to_string());

    assert_eq!(
        selected,
        MonotonicSelection {
            text: original.to_string(),
            original_tokens: token_count(original),
            selected_tokens: token_count(original),
            transformed: false,
        }
    );
}

#[cfg(unix)]
#[test]
fn artifact_and_telemetry_storage_use_private_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().expect("temporary CodexZero directory");
    let artifact = store_raw(temp.path(), b"private output").expect("store private artifact");
    let telemetry_path = temp.path().join("telemetry").join("events.jsonl");
    append_telemetry_to_path(&telemetry_path, &serde_json::json!({"event": "test"}))
        .expect("append private telemetry");

    let modes = (
        std::fs::metadata(temp.path())
            .expect("artifact root metadata")
            .permissions()
            .mode()
            & 0o777,
        std::fs::metadata(temp.path().join("sha256"))
            .expect("artifact directory metadata")
            .permissions()
            .mode()
            & 0o777,
        std::fs::metadata(artifact.path)
            .expect("artifact metadata")
            .permissions()
            .mode()
            & 0o777,
        std::fs::metadata(telemetry_path.parent().expect("telemetry parent"))
            .expect("telemetry directory metadata")
            .permissions()
            .mode()
            & 0o777,
        std::fs::metadata(telemetry_path)
            .expect("telemetry metadata")
            .permissions()
            .mode()
            & 0o777,
    );

    assert_eq!(modes, (0o700, 0o700, 0o600, 0o700, 0o600));
}
