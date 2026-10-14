use crate::sha256;
use anyhow::Context;
use anyhow::Result;
use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::DirBuilderExt;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    pub sha256: String,
    pub byte_count: usize,
    pub path: PathBuf,
}

pub fn store_raw(root: &Path, raw: &[u8]) -> Result<Artifact> {
    let sha256 = sha256(raw);
    let directory = root.join("sha256");
    let path = directory.join(&sha256);
    create_private_dir_all(root)
        .with_context(|| format!("failed to create artifact root {}", root.display()))?;
    create_private_dir_all(&directory).with_context(|| {
        format!(
            "failed to create artifact directory {}",
            directory.display()
        )
    })?;
    if path.exists() {
        let existing = fs::read(&path)
            .with_context(|| format!("failed to verify raw artifact {}", path.display()))?;
        anyhow::ensure!(
            existing == raw,
            "artifact hash collision or corruption at {}",
            path.display()
        );
    } else {
        let temporary = directory.join(format!(
            ".{sha256}.{}.{}.tmp",
            std::process::id(),
            NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = open_private_new(&temporary)
            .with_context(|| format!("failed to create raw artifact {}", temporary.display()))?;
        file.write_all(raw)
            .with_context(|| format!("failed to write raw artifact {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("failed to flush raw artifact {}", temporary.display()))?;
        drop(file);
        if let Err(error) = fs::rename(&temporary, &path) {
            if path.exists() {
                let _ = fs::remove_file(&temporary);
                let existing = fs::read(&path).with_context(|| {
                    format!("failed to verify raced raw artifact {}", path.display())
                })?;
                anyhow::ensure!(
                    existing == raw,
                    "artifact hash collision or corruption at {}",
                    path.display()
                );
            } else {
                return Err(error).with_context(|| {
                    format!(
                        "failed to move raw artifact {} to {}",
                        temporary.display(),
                        path.display()
                    )
                });
            }
        }
    }
    tighten_private_file_permissions(&path)
        .with_context(|| format!("failed to protect raw artifact {}", path.display()))?;

    Ok(Artifact {
        sha256,
        byte_count: raw.len(),
        path,
    })
}

pub fn append_telemetry(record: &serde_json::Value) -> Result<bool> {
    let Some(path) = std::env::var_os("CODEX_ZERO_TELEMETRY_FILE").filter(|path| !path.is_empty())
    else {
        return Ok(false);
    };
    append_telemetry_to_path(&PathBuf::from(path), record)?;
    Ok(true)
}

pub fn append_telemetry_to_path(path: &Path, record: &serde_json::Value) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        create_private_dir_all(parent).with_context(|| {
            format!("failed to create telemetry directory {}", parent.display())
        })?;
    }
    let mut file = open_private_append(path)
        .with_context(|| format!("failed to open telemetry file {}", path.display()))?;
    tighten_private_file_permissions(path)
        .with_context(|| format!("failed to protect telemetry file {}", path.display()))?;
    serde_json::to_writer(&mut file, record).context("failed to serialize telemetry record")?;
    file.write_all(b"\n")
        .context("failed to terminate telemetry record")?;
    Ok(())
}

fn create_private_dir_all(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    fs::create_dir_all(path)?;

    Ok(())
}

fn open_private_new(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path)
}

fn open_private_append(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path)
}

fn tighten_private_file_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
