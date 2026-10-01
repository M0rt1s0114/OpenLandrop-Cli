// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Mortis0114

//! File descriptor generation (send side) and safe materialisation (receive side).

use crate::messages::FileDescriptor;
use anyhow::{Context, Result, anyhow, bail};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Serialises the "pick a free name, then claim it" step across transfers.
///
/// The receiver serves connections concurrently. Without this, two transfers
/// arriving together would each resolve the same free name — neither can see the
/// other's file yet — and then overwrite each other. The claim inside
/// `prepare_incoming` is what makes the pair atomic; this lock is held only for
/// the length of that function.
static RESERVING: Mutex<()> = Mutex::new(());

/// Maximum directory recursion depth, to bound pathological or cyclic trees.
const MAX_DEPTH: usize = 64;

/// A concrete file on disk paired with the abstract name it is offered under.
#[derive(Debug, Clone)]
pub struct FileLeaf {
    pub path: PathBuf,
    pub abstract_name: String,
}

/// Expand the given paths into leaves, recursing into directories.
///
/// A top-level file keeps its basename, and a
/// directory's members are prefixed with `dir/`, recursively.
pub fn parse_paths(inputs: &[PathBuf]) -> Result<Vec<FileLeaf>> {
    let mut out = Vec::new();
    for input in inputs {
        let metadata =
            fs::metadata(input).with_context(|| format!("cannot stat {}", input.display()))?;
        if metadata.is_file() {
            let name = input
                .file_name()
                .ok_or_else(|| anyhow!("{} has no file name", input.display()))?
                .to_string_lossy()
                .to_string();
            out.push(FileLeaf {
                path: input.clone(),
                abstract_name: name,
            });
        } else if metadata.is_dir() {
            collect_dir(input, "", 0, &mut out)?;
        }
        // Anything else (fifo, socket, device) is skipped.
    }
    if out.is_empty() {
        bail!("nothing to send: no regular files found in the given paths");
    }
    Ok(out)
}

fn collect_dir(dir: &Path, prefix: &str, depth: usize, out: &mut Vec<FileLeaf>) -> Result<()> {
    if depth >= MAX_DEPTH {
        bail!("directory nesting too deep at {}", dir.display());
    }
    let base = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let prefix = format!("{prefix}{base}/");

    let entries =
        fs::read_dir(dir).with_context(|| format!("cannot read directory {}", dir.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        // unreadable entry: skip rather than abort the transfer
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if metadata.is_file() {
            let name = entry.file_name().to_string_lossy().to_string();
            out.push(FileLeaf {
                path,
                abstract_name: format!("{prefix}{name}"),
            });
        } else if metadata.is_dir() {
            collect_dir(&path, &prefix, depth + 1, out)?;
        }
    }
    Ok(())
}

/// Build the wire descriptor for a leaf.
pub fn describe(leaf: &FileLeaf) -> Result<FileDescriptor> {
    let metadata =
        fs::metadata(&leaf.path).with_context(|| format!("cannot stat {}", leaf.path.display()))?;
    let modified = metadata
        .modified()
        .map_err(|e| anyhow!("cannot read mtime of {}: {e}", leaf.path.display()))?;
    let last_modified = modified
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Ok(FileDescriptor {
        filename: leaf.abstract_name.clone(),
        size: metadata.len(),
        last_modified,
        permissions: mode_to_permissions(&metadata),
    })
}

pub fn total_size(files: &[FileDescriptor]) -> u64 {
    files.iter().map(|f| f.size).sum()
}

/// Native mode bits -> the `"644"`-style octal string the protocol uses.
#[cfg(unix)]
fn mode_to_permissions(metadata: &fs::Metadata) -> String {
    use std::os::unix::fs::PermissionsExt;
    format!("{:03o}", metadata.permissions().mode() & 0o777)
}

/// Windows has no Unix mode, so the mode that is sent is a fixed answer: `0666`
/// for a regular file and `0444` when it is read-only.
#[cfg(not(unix))]
fn mode_to_permissions(metadata: &fs::Metadata) -> String {
    if metadata.permissions().readonly() {
        "444".to_string()
    } else {
        "666".to_string()
    }
}

/// `"644"` -> native mode bits.
pub fn permissions_to_mode(permissions: &str) -> u32 {
    u32::from_str_radix(permissions.trim(), 8).unwrap_or(0o644) & 0o777
}

// ---------------------------------------------------------------------------
// Receive side
// ---------------------------------------------------------------------------

/// Reject anything that could escape the download directory.
///
/// The desktop app simply joins the sender-supplied name onto its download path,
/// so a hostile sender can write outside it with `../../`. A CLI must not.
pub fn sanitize_abstract_name(name: &str) -> Result<PathBuf> {
    if name.is_empty() {
        bail!("file_send_request contains an empty filename");
    }
    if name.len() > 4096 {
        bail!("filename is unreasonably long");
    }
    let mut out = PathBuf::new();
    for component in name.split(['/', '\\']) {
        if component.is_empty() || component == "." {
            continue;
        }
        if component == ".." {
            bail!("rejected path traversal in filename {name:?}");
        }
        if component.contains('\0') {
            bail!("rejected NUL byte in filename {name:?}");
        }
        if component.chars().any(char::is_control) {
            bail!("rejected control character in filename {name:?}");
        }
        // ':' enables Windows drive-relative paths and NTFS alternate data streams.
        if component.contains(':') {
            bail!("rejected ':' in filename {name:?}");
        }
        if component.len() > 255 {
            bail!("filename component too long in {name:?}");
        }
        out.push(component);
    }
    if out.as_os_str().is_empty() {
        bail!("filename {name:?} resolves to nothing");
    }
    Ok(out)
}

/// `"a.tar.gz"` + 2 -> `"a (2).tar.gz"`: the counter goes before the *first* dot,
/// so the extension stays whole however many dots the name has.
fn with_counter(name: &str, counter: u32) -> String {
    match name.find('.') {
        Some(index) => format!("{} ({}){}", &name[..index], counter, &name[index..]),
        None => format!("{name} ({counter})"),
    }
}

/// Pick a name that does not collide with an existing entry under `dir`.
fn unique_top_level(dir: &Path, name: &str) -> String {
    if !dir.join(name).exists() {
        return name.to_string();
    }
    for counter in 2..100_000u32 {
        let candidate = with_counter(name, counter);
        if !dir.join(&candidate).exists() {
            return candidate;
        }
    }
    format!("{name}.{}", std::process::id())
}

/// A file about to be written, with its collision-free destination resolved.
#[derive(Debug, Clone)]
pub struct IncomingFile {
    pub descriptor: FileDescriptor,
    pub target: PathBuf,
}

/// Resolve destinations for an incoming offer and create the parent directories.
///
/// Collision handling is keyed on the first path segment and shared across the
/// whole request, so `docs/a` and `docs/b` land in the same renamed directory.
pub fn prepare_incoming(dir: &Path, files: &[FileDescriptor]) -> Result<Vec<IncomingFile>> {
    fs::create_dir_all(dir)
        .with_context(|| format!("cannot create download directory {}", dir.display()))?;

    // Held for the whole resolution: every transfer that lands while this one is
    // choosing would otherwise be choosing from the same set of free names.
    let _reserving = RESERVING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let mut taken: HashMap<String, String> = HashMap::new();
    let mut out = Vec::with_capacity(files.len());

    for descriptor in files {
        let relative = sanitize_abstract_name(&descriptor.filename)?;
        let mut components = relative.components();

        let first = components
            .next()
            .ok_or_else(|| anyhow!("filename {:?} resolves to nothing", descriptor.filename))?;
        let first_str = first.as_os_str().to_string_lossy().to_string();

        let mapped = match taken.get(&first_str) {
            Some(existing) => existing.clone(),
            None => {
                let resolved = unique_top_level(dir, &first_str);
                taken.insert(first_str, resolved.clone());
                resolved
            }
        };

        let mut target = dir.join(&mapped);
        for component in components {
            target.push(component);
        }

        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("cannot create directory {}", parent.display()))?;
        }

        // Claim the name on disk. `unique_top_level` only sees files that already
        // exist, so without this a concurrent transfer would pick the same name and
        // both writes would race to the same path. The receiver opens with
        // `File::create`, which truncates this placeholder.
        if !target.exists() {
            fs::File::create(&target)
                .with_context(|| format!("cannot reserve {}", target.display()))?;
        }

        out.push(IncomingFile {
            descriptor: descriptor.clone(),
            target,
        });
    }
    Ok(out)
}

/// Apply the sender's mtime and permission bits to a written file.
pub fn apply_metadata(path: &Path, descriptor: &FileDescriptor) -> Result<()> {
    if descriptor.last_modified > 0 {
        let modified =
            std::time::UNIX_EPOCH + std::time::Duration::from_secs(descriptor.last_modified as u64);
        if let Ok(file) = fs::OpenOptions::new().write(true).open(path) {
            let _ = file.set_modified(modified);
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = permissions_to_mode(&descriptor.permissions);
        if mode != 0 {
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(name: &str, size: u64) -> FileDescriptor {
        FileDescriptor {
            filename: name.to_string(),
            size,
            last_modified: 1_700_000_000,
            permissions: "644".to_string(),
        }
    }

    #[test]
    fn counter_goes_before_the_first_dot() {
        assert_eq!(with_counter("a.txt", 2), "a (2).txt");
        assert_eq!(with_counter("a.tar.gz", 3), "a (3).tar.gz");
        assert_eq!(with_counter("noext", 2), "noext (2)");
    }

    #[test]
    fn rejects_path_traversal() {
        assert!(sanitize_abstract_name("../../etc/passwd").is_err());
        assert!(sanitize_abstract_name("..\\..\\windows\\system32\\evil.dll").is_err());
        assert!(sanitize_abstract_name("docs/../../../escape").is_err());
        assert!(sanitize_abstract_name("a/../../b").is_err());
    }

    #[test]
    fn rejects_absolute_and_exotic_names() {
        assert!(sanitize_abstract_name("").is_err());
        assert!(sanitize_abstract_name("C:/windows/system32/evil").is_err());
        assert!(sanitize_abstract_name("file:stream").is_err());
        assert!(sanitize_abstract_name("bad\0name").is_err());
        assert!(sanitize_abstract_name("bad\nname").is_err());
    }

    #[test]
    fn accepts_and_normalises_legitimate_names() {
        assert_eq!(
            sanitize_abstract_name("docs/a.txt").unwrap(),
            PathBuf::from("docs/a.txt")
        );
        assert_eq!(
            sanitize_abstract_name("./docs//b.txt").unwrap(),
            PathBuf::from("docs/b.txt")
        );
        assert_eq!(
            sanitize_abstract_name("docs\\c.txt").unwrap(),
            PathBuf::from("docs/c.txt")
        );
    }

    #[test]
    fn permissions_round_trip() {
        assert_eq!(permissions_to_mode("644"), 0o644);
        assert_eq!(permissions_to_mode("755"), 0o755);
        assert_eq!(permissions_to_mode("garbage"), 0o644);
    }

    #[test]
    fn prepare_incoming_dedupes_shared_directory_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let files = vec![descriptor("docs/a.txt", 1), descriptor("docs/b.txt", 2)];
        let first = prepare_incoming(root, &files).unwrap();
        assert_eq!(first[0].target, root.join("docs").join("a.txt"));
        assert_eq!(first[1].target, root.join("docs").join("b.txt"));

        // A second transfer of the same tree must land in "docs (2)", and both
        // members must agree on that name.
        fs::create_dir_all(root.join("docs")).unwrap();
        let second = prepare_incoming(root, &files).unwrap();
        assert_eq!(second[0].target, root.join("docs (2)").join("a.txt"));
        assert_eq!(second[1].target, root.join("docs (2)").join("b.txt"));
    }

    #[test]
    fn prepare_incoming_never_escapes_the_target_directory() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("downloads");
        let files = vec![descriptor("../escaped.txt", 1)];
        assert!(prepare_incoming(&root, &files).is_err());
        assert!(!dir.path().join("escaped.txt").exists());
    }

    #[test]
    fn parse_paths_expands_directories() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("tree/sub")).unwrap();
        fs::write(root.join("tree/top.txt"), b"a").unwrap();
        fs::write(root.join("tree/sub/deep.txt"), b"bb").unwrap();

        let leaves = parse_paths(&[root.join("tree")]).unwrap();
        let mut names: Vec<_> = leaves.iter().map(|l| l.abstract_name.clone()).collect();
        names.sort();
        assert_eq!(names, vec!["tree/sub/deep.txt", "tree/top.txt"]);

        let described: Vec<_> = leaves.iter().map(|l| describe(l).unwrap()).collect();
        assert_eq!(total_size(&described), 3);
    }

    #[test]
    fn parse_paths_rejects_empty_input() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("empty")).unwrap();
        assert!(parse_paths(&[dir.path().join("empty")]).is_err());
    }

    #[test]
    fn concurrent_arrivals_claim_different_names() {
        let dir = tempfile::tempdir().unwrap();
        let descriptor = FileDescriptor {
            filename: "same.txt".to_string(),
            size: 4,
            last_modified: 0,
            permissions: "644".to_string(),
        };

        // Two devices sending a file of the same name at the same time. Neither
        // can see the other's file when it picks a destination, so the name has to
        // be claimed as it is chosen or both writes land on one path.
        let first = prepare_incoming(dir.path(), std::slice::from_ref(&descriptor)).unwrap();
        let second = prepare_incoming(dir.path(), std::slice::from_ref(&descriptor)).unwrap();

        assert_eq!(first[0].target, dir.path().join("same.txt"));
        assert_eq!(second[0].target, dir.path().join("same (2).txt"));
        assert!(
            first[0].target.exists() && second[0].target.exists(),
            "each destination must be claimed on disk"
        );
    }
}
