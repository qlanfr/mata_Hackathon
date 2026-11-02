//! Candidate workspaces: a fresh copy of the target source per trial, never the original.

use std::path::Path;

use aiops_core::patch::FileChange;
use sha2::{Digest, Sha256};

/// Recursively copies `src` into `dst`, skipping build output, VCS data and symlinks.
pub fn copy_tree(src: &Path, dst: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "target" || name == ".git" {
            continue;
        }
        let kind = entry.file_type()?;
        let to = dst.join(&name);
        if kind.is_symlink() {
            continue;
        } else if kind.is_dir() {
            copy_tree(&entry.path(), &to)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

/// Hash of every file under `dir` (paths and contents), ignoring build output.
pub fn source_hash(dir: &Path) -> anyhow::Result<String> {
    let mut files = Vec::new();
    collect(dir, dir, &mut files)?;
    files.sort();
    let mut hasher = Sha256::new();
    for rel in files {
        hasher.update(rel.as_bytes());
        hasher.update([0]);
        hasher.update(std::fs::read(dir.join(&rel))?);
        hasher.update([0]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<String>) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "target" || name == ".git" {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect(root, &entry.path(), out)?;
        } else if kind.is_file() {
            out.push(
                entry
                    .path()
                    .strip_prefix(root)?
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    Ok(())
}

pub fn unified_diff(changes: &[FileChange]) -> String {
    changes
        .iter()
        .map(|c| {
            similar::TextDiff::from_lines(&c.before, &c.after)
                .unified_diff()
                .context_radius(3)
                .header(&format!("a/{}", c.path), &format!("b/{}", c.path))
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}
