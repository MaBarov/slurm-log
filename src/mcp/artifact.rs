//! Bounded, owner/job-bound result-artifact search beneath a configured root.
//!
//! Every open is descriptor-relative beneath the configured root, symlinks
//! and hard links are refused, per-file size and total output are bounded,
//! and content is only returned for MIME-sniffed text-like payloads.

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::secure_open::SecureDir;

/// Maximum bytes of artifact content returned per match.
pub const MAX_ARTIFACT_CONTENT: usize = 256 * 1024;
/// Files larger than this are listed as oversized with no hash or content.
pub const MAX_ARTIFACT_SIZE: u64 = 64 * 1024 * 1024;
/// Total bytes of JSON payload the whole result may carry.
pub const MAX_RESULT_BYTES: usize = 1024 * 1024;

const MAX_MATCHES: usize = 200;
const MAX_WALK_ENTRIES: usize = 20_000;
const MAX_DEPTH: usize = 12;

pub struct ArtifactSearch {
    pub matches: Vec<Value>,
    pub scanned: usize,
    pub truncated: bool,
}

/// Search `subdir` (relative to `root`) recursively for files whose basename
/// matches `pattern` (`*` and `?` globs). Every read is confined beneath
/// `root` with descriptor-relative opens.
pub fn search(
    root: &Path,
    subdir: &Path,
    pattern: &str,
    content_max: usize,
) -> Result<ArtifactSearch> {
    let base = SecureDir::open_root(root)?;
    let cursor = if subdir.as_os_str().is_empty() || subdir == Path::new(".") {
        base
    } else {
        base.open_directory(subdir)
            .with_context(|| format!("securely open search root {}", subdir.display()))?
    };
    let mut stack = vec![(PathBuf::new(), cursor, 0_usize)];
    let mut matches = Vec::new();
    let mut scanned = 0_usize;
    let mut truncated = false;
    let mut result_bytes = 0_usize;
    'walk: while let Some((relative, directory, depth)) = stack.pop() {
        let mut entries: Vec<_> = fs::read_dir(directory.proc_path())?
            .filter_map(Result::ok)
            .collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries.into_iter().rev() {
            scanned += 1;
            if scanned > MAX_WALK_ENTRIES {
                truncated = true;
                break 'walk;
            }
            let name = entry.file_name();
            let child = relative.join(&name);
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                if depth < MAX_DEPTH
                    && let Ok(next) = directory.open_directory(Path::new(&name))
                {
                    stack.push((child, next, depth + 1));
                }
                continue;
            }
            if !file_type.is_file() || !glob_match(pattern, &name.to_string_lossy()) {
                continue;
            }
            if matches.len() == MAX_MATCHES {
                truncated = true;
                break 'walk;
            }
            if let Some(value) = read_match(
                &directory,
                Path::new(&name),
                &child,
                content_max,
                &mut result_bytes,
            ) {
                matches.push(value);
            }
            if result_bytes >= MAX_RESULT_BYTES {
                truncated = true;
                break 'walk;
            }
        }
    }
    Ok(ArtifactSearch {
        matches,
        scanned,
        truncated,
    })
}

fn read_match(
    directory: &SecureDir,
    name: &Path,
    relative: &Path,
    content_max: usize,
    result_bytes: &mut usize,
) -> Option<Value> {
    let file = directory.open_file(name).ok()?;
    let metadata = file.metadata().ok()?;
    let expected = metadata.len();
    let mut value = json!({
        "path": relative.display().to_string(),
        "name": name.display().to_string(),
        "size": expected,
    });
    let mut bytes = Vec::with_capacity(expected.min(MAX_ARTIFACT_SIZE.saturating_add(1)) as usize);
    let mut reader = file.take(MAX_ARTIFACT_SIZE.saturating_add(1));
    let read = reader.read_to_end(&mut bytes).ok()?;
    if read as u64 > MAX_ARTIFACT_SIZE {
        value["oversized"] = Value::Bool(true);
        *result_bytes += 96;
        return Some(value);
    }
    let mime = mime_sniff(&bytes);
    let hash = sha256_hex(&bytes);
    value["mime_type"] = Value::String(mime.into());
    value["sha256"] = Value::String(hash);
    match text_content(mime, &bytes, content_max) {
        Some((content, cut)) => {
            value["content"] = Value::String(content);
            value["content_truncated"] = Value::Bool(cut);
            *result_bytes = result_bytes.saturating_add(bytes.len().min(MAX_RESULT_BYTES));
        }
        None => {
            value["content"] = Value::Null;
            value["content_omitted"] = Value::String("binary".into());
            *result_bytes += 96;
        }
    }
    Some(value)
}

fn text_content(mime: &str, bytes: &[u8], maximum: usize) -> Option<(String, bool)> {
    if !matches!(mime, "application/json" | "text/plain") {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let truncated = text.len() > maximum;
    let mut end = text.len().min(maximum);
    while !text.is_char_boundary(end) {
        end += 1;
    }
    Some((text[..end].to_string(), truncated))
}

pub fn mime_sniff(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return "image/png";
    }
    if bytes.starts_with(&[0x1F, 0x8B]) {
        return "application/gzip";
    }
    if bytes.starts_with(b"PK\x03\x04") {
        return "application/zip";
    }
    if bytes.starts_with(b"%PDF") {
        return "application/pdf";
    }
    if bytes.starts_with(&[0x7F, b'E', b'L', b'F']) {
        return "application/x-executable";
    }
    let trimmed = bytes
        .iter()
        .copied()
        .skip_while(|byte| byte.is_ascii_whitespace())
        .collect::<Vec<_>>();
    if trimmed.starts_with(b"{") || trimmed.starts_with(b"[") {
        return "application/json";
    }
    match std::str::from_utf8(bytes) {
        Ok(text) if !text.contains('\0') => "text/plain",
        _ => "application/octet-stream",
    }
}

/// Match a basename against a `*`/`?` glob. `*` matches any run of characters,
/// `?` matches exactly one character; everything else must match literally.
pub fn glob_match(pattern: &str, name: &str) -> bool {
    fn matches(pat: &[u8], text: &[u8]) -> bool {
        match pat {
            [] => text.is_empty(),
            [b'*', rest @ ..] => {
                matches(rest, text) || text.first().is_some_and(|_| matches(pat, &text[1..]))
            }
            [b'?', rest @ ..] => text.first().is_some_and(|_| matches(rest, &text[1..])),
            [byte, rest @ ..] => text.first() == Some(byte) && matches(rest, &text[1..]),
        }
    }
    if pattern.is_empty() {
        return false;
    }
    matches(pattern.as_bytes(), name.as_bytes())
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Validate the optional relative search root before any filesystem access.
pub fn validate_search_root(value: &str) -> Result<PathBuf> {
    if value.is_empty() || value == "." {
        return Ok(PathBuf::from("."));
    }
    let path = Path::new(value);
    let mut parts = 0_usize;
    for component in path.components() {
        match component {
            std::path::Component::Normal(_) => parts += 1,
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => {
                bail!("search_root must be a relative path below the configured root")
            }
        }
    }
    if parts == 0 {
        bail!("search_root must name a directory below the configured root");
    }
    Ok(path.to_path_buf())
}

#[cfg(test)]
#[path = "artifact/tests.rs"]
mod tests;
