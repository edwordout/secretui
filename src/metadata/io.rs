use super::format::{parse_metadata, validate_metadata};
use super::MAX_METADATA_BYTES;
use crate::domain::MetadataFile;
use anyhow::{Context, Result};
use serde::Serialize;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use tempfile::NamedTempFile;

/// Read and strictly validate a bounded metadata document.
///
/// Version 1 accepts its legacy per-item `content_type` field and discards it.
/// Version 2 rejects that field and every other unknown field.
pub fn read_metadata(path: &Path) -> Result<MetadataFile> {
    let file = File::open(path).with_context(|| format!("read {}", path.display()))?;
    let length = file
        .metadata()
        .with_context(|| format!("inspect {}", path.display()))?
        .len();
    anyhow::ensure!(
        length <= MAX_METADATA_BYTES,
        "metadata input is {length} bytes; maximum is {MAX_METADATA_BYTES} bytes"
    );

    // The take protects against the file growing between metadata() and read().
    let mut bytes = Vec::with_capacity(length.min(MAX_METADATA_BYTES) as usize);
    file.take(MAX_METADATA_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("read {}", path.display()))?;
    anyhow::ensure!(
        bytes.len() as u64 <= MAX_METADATA_BYTES,
        "metadata input exceeds {MAX_METADATA_BYTES} bytes"
    );

    parse_metadata(&bytes)
}

/// Create a deterministic metadata file without replacing an existing path.
pub fn write_metadata(path: &Path, metadata: &MetadataFile) -> Result<()> {
    write_metadata_with_options(path, metadata, false)
}

/// Create deterministic metadata JSON, optionally replacing an existing path.
///
/// The write is atomic within the destination directory. The file is synced,
/// has mode 0600 on Unix, and the parent directory is synced after rename.
pub fn write_metadata_with_options(
    path: &Path,
    metadata: &MetadataFile,
    force: bool,
) -> Result<()> {
    validate_metadata(metadata)?;
    let mut text = serde_json::to_vec_pretty(&metadata.clone().sorted())?;
    text.push(b'\n');
    anyhow::ensure!(
        text.len() as u64 <= MAX_METADATA_BYTES,
        "serialized metadata is {} bytes; maximum is {MAX_METADATA_BYTES} bytes",
        text.len()
    );
    write_restricted(path, &text, force)
}

/// Create a no-clobber, mode-0600 JSON file for a recovery or report document.
pub fn create_restricted_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut text = serde_json::to_vec_pretty(value)?;
    text.push(b'\n');
    write_restricted(path, &text, false)
}

/// Atomically replace a report file with a new mode-0600 JSON representation.
pub fn replace_restricted_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut text = serde_json::to_vec_pretty(value)?;
    text.push(b'\n');
    write_restricted(path, &text, true)
}

fn write_restricted(path: &Path, bytes: &[u8], force: bool) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    if !force {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                anyhow::bail!("refusing existing symlink {}", path.display())
            }
            Ok(_) => anyhow::bail!(
                "refusing to replace existing path {}; use --force when appropriate",
                path.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
        }
    } else if let Ok(metadata) = fs::symlink_metadata(path) {
        anyhow::ensure!(
            !metadata.file_type().is_dir(),
            "refusing to replace directory {}",
            path.display()
        );
    }

    let mut temporary = NamedTempFile::new_in(parent)
        .with_context(|| format!("create temporary file in {}", parent.display()))?;
    set_private_permissions(temporary.as_file())?;
    temporary
        .write_all(bytes)
        .with_context(|| format!("write temporary file for {}", path.display()))?;
    temporary
        .as_file()
        .sync_all()
        .with_context(|| format!("sync temporary file for {}", path.display()))?;

    if force {
        temporary
            .persist(path)
            .map_err(|error| error.error)
            .with_context(|| format!("replace {}", path.display()))?;
    } else {
        temporary
            .persist_noclobber(path)
            .map_err(|error| error.error)
            .with_context(|| format!("create {} without replacing it", path.display()))?;
    }

    // Re-open without following a stale pre-rename handle and enforce the mode.
    let output = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("open completed file {}", path.display()))?;
    set_private_permissions(&output)?;
    output
        .sync_all()
        .with_context(|| format!("sync completed file {}", path.display()))?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("sync directory {}", parent.display()))?;
    Ok(())
}

#[cfg(unix)]
fn set_private_permissions(file: &File) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .context("set file mode 0600")
}

#[cfg(not(unix))]
fn set_private_permissions(_file: &File) -> Result<()> {
    Ok(())
}
