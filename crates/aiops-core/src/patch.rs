//! Search/replace patches and the policy that decides which files they may touch.
//!
//! Models are poor at producing exact unified diffs, so a patch is a list of edits. Each edit
//! names a file, a `search` string that must occur exactly once, and its `replace` text.
//! Every edit is validated before any file is written, so a rejected patch changes nothing.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Edit {
    pub path: String,
    pub search: String,
    pub replace: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PatchProposal {
    /// One-line description shown to people. Not part of the patch hash.
    pub summary: String,
    pub edits: Vec<Edit>,
}

impl PatchProposal {
    /// Content hash of the edits only. Two patches that differ only in their summary hash the
    /// same, so a rejected change cannot be retried under a new name.
    pub fn content_hash(&self) -> String {
        let mut bytes = Vec::new();
        for edit in &self.edits {
            for part in [&edit.path, &edit.search, &edit.replace] {
                bytes.extend_from_slice(&(part.len() as u64).to_le_bytes());
                bytes.extend_from_slice(part.as_bytes());
            }
        }
        crate::sha256_hex(bytes)
    }
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq, Serialize, Deserialize)]
pub enum PatchError {
    #[error("patch has no edits")]
    Empty,
    #[error("patch has {0} edits; the scenario allows at most {1}")]
    TooManyEdits(usize, usize),
    #[error("`{0}` is not a plain relative path")]
    InvalidPath(String),
    #[error("`{0}` is not inside the editable paths")]
    NotAllowed(String),
    #[error("`{0}` is explicitly protected")]
    Denied(String),
    #[error("`{0}` goes through a symbolic link")]
    Symlink(String),
    #[error("`{0}` does not exist; patches may only edit existing files")]
    Missing(String),
    #[error("the search text for `{0}` is empty")]
    EmptySearch(String),
    #[error("the search text for `{path}` matched {count} times; it must match exactly once")]
    SearchMismatch { path: String, count: usize },
    #[error("cannot read or write `{path}`: {message}")]
    Io { path: String, message: String },
}

/// Which files (relative to the target's source directory) a patch may edit.
#[derive(Debug, Clone)]
pub struct PathPolicy {
    allowed: Vec<String>,
    denied: Vec<String>,
}

impl PathPolicy {
    pub fn new(allowed: Vec<String>, denied: Vec<String>) -> Self {
        Self { allowed, denied }
    }

    /// Lexical checks only (no file system access).
    pub fn check(&self, rel: &str) -> Result<(), PatchError> {
        let invalid = || PatchError::InvalidPath(rel.to_owned());
        if rel.is_empty() || rel.contains('\\') || rel.contains('\0') {
            return Err(invalid());
        }
        // Check raw segments: `Path::components` silently drops interior `.`, which would let
        // `src/./repo.rs` and `src/repo.rs` name one file under two keys.
        for segment in rel.split('/') {
            // Empty, `.` and `..` segments, and hidden files and directories (e.g.
            // `.cargo/config.toml`, which can change how the build runs) are never editable.
            if segment.is_empty() || segment.starts_with('.') {
                return Err(invalid());
            }
        }
        if !Path::new(rel)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
        {
            return Err(invalid());
        }
        if self.denied.iter().any(|pattern| glob_match(pattern, rel)) {
            return Err(PatchError::Denied(rel.to_owned()));
        }
        if !self.allowed.iter().any(|pattern| glob_match(pattern, rel)) {
            return Err(PatchError::NotAllowed(rel.to_owned()));
        }
        Ok(())
    }
}

/// One file's contents before and after a patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub before: String,
    pub after: String,
}

/// Validates every edit against `policy` and the files under `root`, then writes the result.
/// Nothing is written unless every edit is valid.
pub fn apply_patch(
    root: &Path,
    patch: &PatchProposal,
    policy: &PathPolicy,
    max_edits: usize,
) -> Result<Vec<FileChange>, PatchError> {
    if patch.edits.is_empty() {
        return Err(PatchError::Empty);
    }
    if patch.edits.len() > max_edits {
        return Err(PatchError::TooManyEdits(patch.edits.len(), max_edits));
    }

    // path -> (original, current)
    let mut files: BTreeMap<String, (String, String)> = BTreeMap::new();
    for edit in &patch.edits {
        policy.check(&edit.path)?;
        if edit.search.is_empty() {
            return Err(PatchError::EmptySearch(edit.path.clone()));
        }
        if !files.contains_key(&edit.path) {
            let full = resolve_existing(root, &edit.path)?;
            let text = std::fs::read_to_string(&full).map_err(|e| PatchError::Io {
                path: edit.path.clone(),
                message: e.to_string(),
            })?;
            files.insert(edit.path.clone(), (text.clone(), text));
        }
        let (_, current) = files.get_mut(&edit.path).expect("inserted above");
        let count = current.matches(edit.search.as_str()).count();
        if count != 1 {
            return Err(PatchError::SearchMismatch {
                path: edit.path.clone(),
                count,
            });
        }
        *current = current.replacen(edit.search.as_str(), &edit.replace, 1);
    }

    let mut changes = Vec::with_capacity(files.len());
    for (path, (before, after)) in files {
        let full = root.join(&path);
        std::fs::write(&full, &after).map_err(|e| PatchError::Io {
            path: path.clone(),
            message: e.to_string(),
        })?;
        changes.push(FileChange {
            path,
            before,
            after,
        });
    }
    Ok(changes)
}

/// Joins `rel` onto `root`, refusing symlinks anywhere along the way and missing files.
fn resolve_existing(root: &Path, rel: &str) -> Result<PathBuf, PatchError> {
    let mut current = root.to_path_buf();
    for component in Path::new(rel).components() {
        current.push(component.as_os_str());
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(PatchError::Symlink(rel.to_owned()))
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(PatchError::Missing(rel.to_owned()))
            }
            Err(e) => {
                return Err(PatchError::Io {
                    path: rel.to_owned(),
                    message: e.to_string(),
                })
            }
        }
    }
    if !current.is_file() {
        return Err(PatchError::Missing(rel.to_owned()));
    }
    Ok(current)
}

/// Minimal glob: `**` matches any number of path segments, `*` matches within one segment.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let pattern: Vec<&str> = pattern.split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    match_segments(&pattern, &path)
}

fn match_segments(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|skip| match_segments(rest, &path[skip..])),
        Some((first, rest)) => match path.split_first() {
            Some((segment, path_rest)) => {
                match_wildcard(first, segment) && match_segments(rest, path_rest)
            }
            None => false,
        },
    }
}

fn match_wildcard(pattern: &str, text: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == text,
        Some((prefix, suffix_pattern)) => {
            if !text.starts_with(prefix) {
                return false;
            }
            let rest = &text[prefix.len()..];
            (0..=rest.len())
                .filter(|&i| rest.is_char_boundary(i))
                .any(|i| match_wildcard(suffix_pattern, &rest[i..]))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> PathPolicy {
        PathPolicy::new(
            vec!["src/**/*.rs".into()],
            vec![
                "src/bin/**".into(),
                "Cargo.toml".into(),
                "Cargo.lock".into(),
            ],
        )
    }

    fn edit(path: &str, search: &str, replace: &str) -> Edit {
        Edit {
            path: path.into(),
            search: search.into(),
            replace: replace.into(),
        }
    }

    fn patch(edits: Vec<Edit>) -> PatchProposal {
        PatchProposal {
            summary: "test".into(),
            edits,
        }
    }

    #[test]
    fn glob_rules() {
        assert!(glob_match("src/**/*.rs", "src/repo.rs"));
        assert!(glob_match("src/**/*.rs", "src/a/b/repo.rs"));
        assert!(!glob_match("src/**/*.rs", "src/repo.toml"));
        assert!(!glob_match("src/**/*.rs", "tests/repo.rs"));
        assert!(glob_match("Cargo.*", "Cargo.lock"));
        assert!(glob_match("src/bin/**", "src/bin/x.rs"));
    }

    #[test]
    fn policy_rejects_escapes_and_protected_files() {
        let p = policy();
        assert_eq!(p.check("src/repo.rs"), Ok(()));
        assert_eq!(
            p.check("../src/repo.rs"),
            Err(PatchError::InvalidPath("../src/repo.rs".into()))
        );
        assert_eq!(
            p.check("/etc/passwd"),
            Err(PatchError::InvalidPath("/etc/passwd".into()))
        );
        assert_eq!(
            p.check("src/./repo.rs"),
            Err(PatchError::InvalidPath("src/./repo.rs".into()))
        );
        assert_eq!(
            p.check("src//repo.rs"),
            Err(PatchError::InvalidPath("src//repo.rs".into()))
        );
        assert_eq!(
            p.check("src/.hidden.rs"),
            Err(PatchError::InvalidPath("src/.hidden.rs".into()))
        );
        assert_eq!(
            p.check("Cargo.toml"),
            Err(PatchError::Denied("Cargo.toml".into()))
        );
        assert_eq!(
            p.check("src/bin/evil.rs"),
            Err(PatchError::Denied("src/bin/evil.rs".into()))
        );
        assert_eq!(
            p.check("build.rs"),
            Err(PatchError::NotAllowed("build.rs".into()))
        );
    }

    #[test]
    fn applies_exact_single_match() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/repo.rs"), "fn a() { slow() }\n").unwrap();
        let changes = apply_patch(
            dir.path(),
            &patch(vec![edit("src/repo.rs", "slow()", "fast()")]),
            &policy(),
            4,
        )
        .unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("src/repo.rs")).unwrap(),
            "fn a() { fast() }\n"
        );
    }

    #[test]
    fn rejects_ambiguous_or_missing_search_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/repo.rs"), "x x\n").unwrap();
        let err = apply_patch(
            dir.path(),
            &patch(vec![edit("src/repo.rs", "x", "y")]),
            &policy(),
            4,
        )
        .unwrap_err();
        assert_eq!(
            err,
            PatchError::SearchMismatch {
                path: "src/repo.rs".into(),
                count: 2
            }
        );

        // First edit is valid, second is not: nothing may be written.
        let err = apply_patch(
            dir.path(),
            &patch(vec![
                edit("src/repo.rs", "x x", "z"),
                edit("src/repo.rs", "nope", "q"),
            ]),
            &policy(),
            4,
        )
        .unwrap_err();
        assert!(matches!(err, PatchError::SearchMismatch { count: 0, .. }));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("src/repo.rs")).unwrap(),
            "x x\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_paths() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.rs"), "secret").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("src")).unwrap();
        let err = apply_patch(
            dir.path(),
            &patch(vec![edit("src/secret.rs", "secret", "pwned")]),
            &policy(),
            4,
        )
        .unwrap_err();
        assert_eq!(err, PatchError::Symlink("src/secret.rs".into()));
        assert_eq!(
            std::fs::read_to_string(outside.path().join("secret.rs")).unwrap(),
            "secret"
        );
    }

    #[test]
    fn hash_ignores_summary() {
        let a = patch(vec![edit("src/repo.rs", "a", "b")]);
        let mut b = a.clone();
        b.summary = "renamed".into();
        assert_eq!(a.content_hash(), b.content_hash());
        let c = patch(vec![edit("src/repo.rs", "a", "c")]);
        assert_ne!(a.content_hash(), c.content_hash());
    }

    #[test]
    fn rejects_empty_and_oversized_patches() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            apply_patch(dir.path(), &patch(vec![]), &policy(), 4),
            Err(PatchError::Empty)
        );
        let edits = (0..5).map(|_| edit("src/repo.rs", "a", "b")).collect();
        assert_eq!(
            apply_patch(dir.path(), &patch(edits), &policy(), 4),
            Err(PatchError::TooManyEdits(5, 4))
        );
    }
}
