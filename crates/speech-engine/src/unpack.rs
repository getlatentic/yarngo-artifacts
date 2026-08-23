//! Unpacking an archive that will be executed.
//!
//! This is a security boundary and the rules are ours, not whichever `tar` the
//! host ships. Measured on this machine before this existed: the host tar
//! refused `..`, silently stripped a leading `/` so the entry landed somewhere
//! else inside, created a symlink pointing at `/etc/hosts` without complaint,
//! and extracted the remaining entries anyway after the errors — so a caller
//! checking only that its own file appeared would have seen success.
//!
//! # Why this is not a library
//!
//! `exarch-core` 0.6.0 was evaluated against this module's hostile fixtures
//! (2026-08). It refused traversal, absolute paths, symlinks, hard links,
//! devices, fifos and the decompression bomb — atomically, not by skipping.
//! Three things ruled it out:
//!
//! - Extraction cannot be pinned to one format, and magic-byte sniffing
//!   overrides the extension: a hostile file named `.tar.gz` was routed to its
//!   7z parser. We publish exactly one format, and its zip (a 9.0.0
//!   pre-release), 7z, xz and zstd parsers compile in with no feature gates —
//!   so adopting it would widen the parser surface hostile bytes can reach,
//!   which is the opposite of what a security dependency is for.
//! - A contiguous entry (tar type 7) was accepted as a regular file, where the
//!   allowlist here is Regular and Directory exactly.
//! - It applies its own permission policy — a setuid entry landed stripped but
//!   group-writable — where the rule here is that archive modes are not
//!   consulted at all.
//!
//! This module is a page of code over `tar` and `flate2`, both mature and both
//! already in the tree.

use std::path::{Path, PathBuf};

/// What one runtime archive may contain.
///
/// Generous for code and a manifest, and nowhere near enough to be a way to
/// fill a disk. The dependencies are not in here — those are installed by uv
/// from a lock, and are the hundreds of megabytes.
const MAX_ENTRIES: usize = 4_000;
const MAX_ENTRY_BYTES: u64 = 16 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

/// Unpack an archive, materialising nothing outside the directory given.
///
/// The rules are here rather than in whichever `tar` the host ships, because
/// they differ and the differences are the whole question. Measured on this
/// machine: `..` was refused, a leading `/` was silently stripped so the entry
/// landed somewhere else inside, a symlink pointing at `/etc/hosts` was created
/// without complaint, and the remaining entries were extracted anyway after the
/// errors — so a caller checking only that its file appeared would have seen
/// success.
///
/// Only regular files and directories are created. A symlink, a hard link, a
/// device or a fifo is refused rather than skipped: an archive that contains
/// one is not a runtime that lost a file, it is an archive doing something
/// else.
pub fn extract_under(archive: &[u8], root: &Path) -> Result<(), String> {
    use std::io::Read;

    let root = root
        .canonicalize()
        .map_err(|e| format!("cannot resolve {}: {e}", root.display()))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    let entries = tar
        .entries()
        .map_err(|e| format!("archive could not be read: {e}"))?;

    let mut count = 0usize;
    let mut total = 0u64;
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("archive could not be read: {e}"))?;
        count += 1;
        if count > MAX_ENTRIES {
            return Err(format!("archive has more than {MAX_ENTRIES} entries in it"));
        }

        let kind = entry.header().entry_type();
        if !kind.is_file() && !kind.is_dir() {
            return Err(format!(
                "archive contains a {kind:?}, and a runtime is files and directories"
            ));
        }
        let path = entry
            .path()
            .map_err(|e| format!("archive has an unreadable path in it: {e}"))?
            .into_owned();
        let destination = under(&root, &path)?;

        let size = entry.header().size().unwrap_or(0);
        if size > MAX_ENTRY_BYTES {
            return Err(format!("archive entry {} is larger than {MAX_ENTRY_BYTES} bytes", path.display()));
        }
        total += size;
        if total > MAX_TOTAL_BYTES {
            return Err(format!("archive unpacks to more than {MAX_TOTAL_BYTES} bytes"));
        }

        if kind.is_dir() {
            std::fs::create_dir_all(&destination).map_err(|e| e.to_string())?;
            set_mode(&destination, 0o755)?;
            continue;
        }
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        // Read no more than the header claimed, whatever the stream offers.
        let mut bytes = Vec::new();
        entry
            .by_ref()
            .take(size)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("archive entry {} could not be read: {e}", path.display()))?;
        std::fs::write(&destination, &bytes).map_err(|e| e.to_string())?;
        // The archive's mode is not consulted: nothing in a runtime archive is
        // executed by its own bit — the interpreter runs the code — and setuid
        // or group-write are not things an archive gets to ask for.
        set_mode(&destination, 0o644)?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(|e| e.to_string())
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<(), String> {
    Ok(())
}

/// Where one archived path lands, or why it may not land anywhere.
///
/// Refused rather than repaired. An entry naming somewhere else is not a
/// runtime with an odd layout, and quietly relocating it — which is what
/// stripping a leading slash does — turns a refusal into a surprise.
fn under(root: &Path, path: &Path) -> Result<PathBuf, String> {
    use std::path::Component;
    if path.is_absolute() {
        return Err(format!("entry {} names an absolute path", path.display()));
    }
    let mut destination = root.to_path_buf();
    for part in path.components() {
        match part {
            Component::Normal(part) => destination.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(format!("entry {} climbs out of the runtime", path.display()))
            }
            other => {
                return Err(format!("entry {} contains {other:?}", path.display()))
            }
        }
    }
    // Belt and braces: whatever the components said, the result is underneath.
    if !destination.starts_with(root) {
        return Err(format!("entry {} resolves outside the runtime", path.display()));
    }
    Ok(destination)
}
