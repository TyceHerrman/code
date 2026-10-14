use anyhow::Context;
use anyhow::Result;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use tiktoken_rs::o200k_base_singleton;

mod storage;

pub use storage::Artifact;
pub use storage::append_telemetry;
pub use storage::append_telemetry_to_path;
pub use storage::store_raw;

const RLE_CODEC: &str = "line-rle-v1";
const SUCCESSFUL_CHECK_PROJECTION: &str = "successful-check-v1";
const MIN_PROJECTION_LINES: usize = 80;
const PROJECTION_TAIL_LINES: usize = 24;
const MAX_DIAGNOSTIC_LINES: usize = 56;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonotonicSelection {
    pub text: String,
    pub original_tokens: usize,
    pub selected_tokens: usize,
    pub transformed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalEncoding {
    pub text: String,
    pub codec: Option<&'static str>,
    pub projection: Option<&'static str>,
    pub original_line_count: Option<usize>,
    pub presentation_changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LineRle {
    codec: String,
    runs: Vec<LineRun>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LineRun {
    count: usize,
    text: String,
}

pub fn token_count(text: &str) -> usize {
    o200k_base_singleton()
        .encode_with_special_tokens(text)
        .len()
}

pub fn sha256(raw: &[u8]) -> String {
    format!("{:x}", Sha256::digest(raw))
}

pub fn select_strictly_smaller(original: &str, candidate: String) -> MonotonicSelection {
    let original_tokens = token_count(original);
    let candidate_tokens = token_count(&candidate);
    if candidate_tokens < original_tokens {
        MonotonicSelection {
            text: candidate,
            original_tokens,
            selected_tokens: candidate_tokens,
            transformed: true,
        }
    } else {
        MonotonicSelection {
            text: original.to_string(),
            original_tokens,
            selected_tokens: original_tokens,
            transformed: false,
        }
    }
}

pub fn encode_terminal(raw: &[u8]) -> TerminalEncoding {
    let source = String::from_utf8_lossy(raw);
    let without_presentation = strip_presentation_ansi(&source);
    let presentation_changed = without_presentation != source;
    let normalized = normalize_line_endings_if_smaller(&without_presentation);
    let plain_tokens = token_count(&normalized);
    let rle = encode_line_runs(&normalized);

    if let Ok(rle_text) = serde_json::to_string(&rle)
        && token_count(&rle_text) < plain_tokens
    {
        return TerminalEncoding {
            text: rle_text,
            codec: Some(RLE_CODEC),
            projection: None,
            original_line_count: None,
            presentation_changed,
        };
    }

    TerminalEncoding {
        text: normalized,
        codec: None,
        projection: None,
        original_line_count: None,
        presentation_changed,
    }
}

pub fn encode_command_output(
    raw: &[u8],
    command: Option<&str>,
    exit_code: Option<i32>,
) -> TerminalEncoding {
    let encoded = encode_terminal(raw);
    let Some(command) = command else {
        return encoded;
    };
    let Some((projection, original_line_count)) =
        project_successful_check(&String::from_utf8_lossy(raw), command, exit_code)
    else {
        return encoded;
    };
    if token_count(&projection) >= token_count(&encoded.text) {
        return encoded;
    }

    TerminalEncoding {
        text: projection,
        codec: None,
        projection: Some(SUCCESSFUL_CHECK_PROJECTION),
        original_line_count: Some(original_line_count),
        presentation_changed: encoded.presentation_changed,
    }
}

pub fn decode_terminal(encoded: &TerminalEncoding) -> Result<String> {
    if let Some(projection) = encoded.projection {
        anyhow::bail!("{projection} is a model projection; read the raw artifact for exact output");
    }
    match encoded.codec {
        Some(RLE_CODEC) => {
            let rle: LineRle =
                serde_json::from_str(&encoded.text).context("invalid line-rle payload")?;
            let mut decoded = String::new();
            for run in rle.runs {
                for _ in 0..run.count {
                    decoded.push_str(&run.text);
                }
            }
            Ok(decoded)
        }
        Some(codec) => anyhow::bail!("unsupported terminal codec {codec}"),
        None => Ok(encoded.text.clone()),
    }
}

fn project_successful_check(
    output: &str,
    command: &str,
    exit_code: Option<i32>,
) -> Option<(String, usize)> {
    if exit_code != Some(0) || !is_check_command(command) {
        return None;
    }
    let normalized = normalize_line_endings_if_smaller(&strip_presentation_ansi(output));
    let lines = normalized.lines().collect::<Vec<_>>();
    if lines.len() < MIN_PROJECTION_LINES {
        return None;
    }

    let mut selected = vec![false; lines.len()];
    for selected_line in selected.iter_mut().take(lines.len().min(3)) {
        *selected_line = true;
    }
    let mut diagnostic_count = 0;
    for (index, line) in lines.iter().enumerate() {
        if diagnostic_count >= MAX_DIAGNOSTIC_LINES || !is_diagnostic_line(line) {
            continue;
        }
        for selected_line in selected
            .iter_mut()
            .take((index + 1).min(lines.len() - 1) + 1)
            .skip(index.saturating_sub(1))
        {
            if !*selected_line {
                *selected_line = true;
                diagnostic_count += 1;
            }
        }
    }
    for selected_line in selected
        .iter_mut()
        .skip(lines.len().saturating_sub(PROJECTION_TAIL_LINES))
    {
        *selected_line = true;
    }

    let mut projected = format!(
        "Successful check output projected from {} lines; full output is in the attached artifact.\n",
        lines.len()
    );
    let mut previous_index = None;
    let mut previous_line = None;
    let mut repeat_count = 0;
    for (index, line) in lines.iter().copied().enumerate() {
        if !selected[index] {
            continue;
        }
        if previous_index.is_some_and(|previous| index > previous + 1) {
            flush_projected_line(&mut projected, previous_line.take(), repeat_count);
            repeat_count = 0;
            projected.push_str("[...]\n");
        }
        if previous_line == Some(line) {
            repeat_count += 1;
        } else {
            flush_projected_line(&mut projected, previous_line.replace(line), repeat_count);
            repeat_count = 1;
        }
        previous_index = Some(index);
    }
    flush_projected_line(&mut projected, previous_line, repeat_count);
    Some((projected, lines.len()))
}

fn flush_projected_line(output: &mut String, line: Option<&str>, count: usize) {
    let Some(line) = line else {
        return;
    };
    output.push_str(line);
    if count > 1 {
        output.push_str(&format!(" [repeated {count} times]"));
    }
    output.push('\n');
}

fn is_check_command(command: &str) -> bool {
    let command = command.to_ascii_lowercase();
    [
        "cargo test",
        "cargo check",
        "cargo clippy",
        "npm test",
        "npm run test",
        "npm run build",
        "npm run check",
        "npm run lint",
        "pnpm test",
        "pnpm run test",
        "pnpm run build",
        "pnpm run check",
        "pnpm run lint",
        "yarn test",
        "yarn build",
        "yarn lint",
        "node --test",
        "pytest",
        "python -m pytest",
        "go test",
        "dotnet test",
        "gradle test",
        "gradlew test",
        "mvn test",
        "mvn verify",
        "swift test",
    ]
    .iter()
    .any(|pattern| command.contains(pattern))
}

fn is_diagnostic_line(line: &str) -> bool {
    let line = line.to_ascii_lowercase();
    [
        "error",
        "warning",
        "failed",
        "failure",
        "passed",
        "pass ",
        "test result",
        "tests:",
        "summary",
        "success",
    ]
    .iter()
    .any(|marker| line.contains(marker))
}

fn normalize_line_endings_if_smaller(text: &str) -> String {
    if !text.contains('\r') {
        return text.to_string();
    }

    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    if token_count(&normalized) < token_count(text) {
        normalized
    } else {
        text.to_string()
    }
}

fn encode_line_runs(text: &str) -> LineRle {
    let mut runs: Vec<LineRun> = Vec::new();
    for line in text.split_inclusive('\n') {
        if let Some(previous) = runs.last_mut()
            && previous.text == line
        {
            previous.count += 1;
        } else {
            runs.push(LineRun {
                count: 1,
                text: line.to_string(),
            });
        }
    }

    if text.is_empty() {
        runs.clear();
    }

    LineRle {
        codec: RLE_CODEC.to_string(),
        runs,
    }
}

fn strip_presentation_ansi(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    let mut active_hyperlink: Option<String> = None;

    while index < bytes.len() {
        if bytes[index] != 0x1b {
            output.push(bytes[index]);
            index += 1;
            continue;
        }

        if index + 1 >= bytes.len() {
            output.push(bytes[index]);
            break;
        }

        match bytes[index + 1] {
            b'[' => {
                let sequence_start = index;
                index += 2;
                let mut final_byte = None;
                while index < bytes.len() {
                    let byte = bytes[index];
                    index += 1;
                    if (0x40..=0x7e).contains(&byte) {
                        final_byte = Some(byte);
                        break;
                    }
                }
                if final_byte != Some(b'm') {
                    output.extend_from_slice(&bytes[sequence_start..index]);
                }
            }
            b']' => {
                let sequence_start = index;
                let content_start = index + 2;
                index = content_start;
                while index < bytes.len()
                    && bytes[index] != 0x07
                    && !(bytes[index] == 0x1b
                        && index + 1 < bytes.len()
                        && bytes[index + 1] == b'\\')
                {
                    index += 1;
                }
                let content = String::from_utf8_lossy(&bytes[content_start..index]);
                let is_hyperlink = content.starts_with("8;;");
                if let Some(link) = content.strip_prefix("8;;") {
                    if link.is_empty() {
                        if let Some(url) = active_hyperlink.take() {
                            output.extend_from_slice(b" (");
                            output.extend_from_slice(url.as_bytes());
                            output.push(b')');
                        }
                    } else {
                        active_hyperlink = Some(link.to_string());
                    }
                }
                if index < bytes.len() && bytes[index] == 0x07 {
                    index += 1;
                } else if index + 1 < bytes.len() {
                    index += 2;
                }
                if !is_hyperlink {
                    output.extend_from_slice(&bytes[sequence_start..index]);
                }
            }
            _ => {
                output.extend_from_slice(&bytes[index..index + 2]);
                index += 2;
            }
        }
    }

    if let Some(url) = active_hyperlink {
        output.extend_from_slice(b" (");
        output.extend_from_slice(url.as_bytes());
        output.push(b')');
    }

    String::from_utf8_lossy(&output).into_owned()
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
