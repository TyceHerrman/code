use crate::memory_usage::shell_script_for_invocation;
use crate::tools::context::ToolInvocation;
use codex_protocol::parse_command::ParsedCommand;
use codex_shell_command::parse_command::parse_shell_script;

pub(crate) async fn source_fingerprint(invocation: &ToolInvocation) -> Option<String> {
    let script = shell_script_for_invocation(invocation)?;
    let environment = invocation
        .step_context
        .environments
        .turn_environments()
        .next()?;
    if environment.environment.is_remote() {
        return None;
    }
    let cwd = environment.cwd().to_abs_path().ok()?;
    let commands = parse_shell_script(&script);
    if !commands.is_empty()
        && commands
            .iter()
            .all(|command| matches!(command, ParsedCommand::Read { .. }))
    {
        return file_source_fingerprint(&commands, cwd.as_path()).await;
    }
    if !commands.is_empty()
        && commands.iter().all(|command| {
            matches!(
                command,
                ParsedCommand::Search { .. } | ParsedCommand::ListFiles { .. }
            )
        })
    {
        return repository_search_fingerprint(&commands, cwd.as_path()).await;
    }

    match script.trim() {
        "git status"
        | "git status --short"
        | "git status --porcelain"
        | "git status --porcelain=v1" => repository_state_fingerprint(cwd.as_path()).await,
        _ => None,
    }
}

pub(crate) async fn file_source_fingerprint(
    commands: &[ParsedCommand],
    cwd: &std::path::Path,
) -> Option<String> {
    let mut sources = Vec::with_capacity(commands.len());
    for command in commands {
        let ParsedCommand::Read { path, .. } = command else {
            return None;
        };
        let path = if path.is_absolute() {
            path.clone()
        } else {
            cwd.join(path)
        };
        let bytes = tokio::fs::read(&path).await.ok()?;
        sources.push(serde_json::json!({
            "path": path,
            "blob_sha256": codex_zero_codec::sha256(&bytes),
            "byte_range": [0, bytes.len()],
        }));
    }
    serde_json::to_vec(&sources)
        .ok()
        .map(|bytes| codex_zero_codec::sha256(&bytes))
}

pub(crate) async fn repository_search_fingerprint(
    commands: &[ParsedCommand],
    cwd: &std::path::Path,
) -> Option<String> {
    let root = repository_root(cwd).await?;
    let canonical_root = tokio::fs::canonicalize(&root).await.ok()?;
    let canonical_cwd = tokio::fs::canonicalize(cwd).await.ok()?;
    if !canonical_cwd.starts_with(&canonical_root) {
        return None;
    }

    for command in commands {
        let (cmd, path) = match command {
            ParsedCommand::Search { cmd, path, .. } | ParsedCommand::ListFiles { cmd, path } => {
                (cmd, path.as_deref())
            }
            ParsedCommand::Read { .. } | ParsedCommand::Unknown { .. } => return None,
        };
        if !supported_repository_search(cmd) {
            return None;
        }
        let scope = path
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let scope = if scope.is_absolute() {
            scope
        } else {
            cwd.join(scope)
        };
        let canonical_scope = tokio::fs::canonicalize(scope).await.ok()?;
        if !canonical_scope.starts_with(&canonical_root) {
            return None;
        }
    }

    repository_state_fingerprint(&root).await
}

fn supported_repository_search(command: &str) -> bool {
    let mut words = command.split_whitespace();
    let executable = words.next().and_then(|word| {
        std::path::Path::new(word)
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
    });
    let is_ripgrep = matches!(executable, Some("rg" | "rg.exe"));
    let is_git_read =
        executable == Some("git") && matches!(words.next(), Some("grep" | "ls-files"));
    if !is_ripgrep && !is_git_read {
        return false;
    }

    !command.split_whitespace().any(|word| {
        matches!(
            word,
            "--hidden" | "--follow" | "-L" | "-u" | "-uu" | "-uuu" | "--pre"
        ) || word.starts_with("--no-ignore")
            || word.starts_with("--pre=")
    })
}

pub(crate) async fn repository_state_fingerprint(cwd: &std::path::Path) -> Option<String> {
    let root = repository_root(cwd).await?;
    let status = git(
        &root,
        &["status", "--porcelain=v2", "-z", "--untracked-files=all"],
    )
    .await?;
    let paths = git(
        &root,
        &[
            "ls-files",
            "-m",
            "-o",
            "--deleted",
            "--exclude-standard",
            "-z",
        ],
    )
    .await?;
    let head = git(&root, &["rev-parse", "HEAD"]).await.unwrap_or_default();
    let index = git(&root, &["ls-files", "--stage", "-z"]).await?;

    let mut changed_files = Vec::new();
    for raw_path in paths
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let relative = std::str::from_utf8(raw_path).ok()?;
        let path = root.join(relative);
        let state = match tokio::fs::read(&path).await {
            Ok(bytes) => serde_json::json!({
                "path": relative,
                "blob_sha256": codex_zero_codec::sha256(&bytes),
                "byte_range": [0, bytes.len()],
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                serde_json::json!({"path": relative, "deleted": true})
            }
            Err(_) => return None,
        };
        changed_files.push(state);
    }
    changed_files.sort_by_key(std::string::ToString::to_string);
    let fingerprint = serde_json::json!({
        "status_sha256": codex_zero_codec::sha256(&status),
        "head_sha256": codex_zero_codec::sha256(&head),
        "index_sha256": codex_zero_codec::sha256(&index),
        "changed_files": changed_files,
    });
    serde_json::to_vec(&fingerprint)
        .ok()
        .map(|bytes| codex_zero_codec::sha256(&bytes))
}

async fn repository_root(cwd: &std::path::Path) -> Option<std::path::PathBuf> {
    let root = git(cwd, &["rev-parse", "--show-toplevel"]).await?;
    let root = std::str::from_utf8(&root).ok()?.trim();
    Some(std::path::PathBuf::from(root))
}

async fn git(cwd: &std::path::Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .await
        .ok()?;
    output.status.success().then_some(output.stdout)
}
