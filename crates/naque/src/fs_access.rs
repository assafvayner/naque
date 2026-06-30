//! Filesystem read authorization.
//!
//! This is a permission dimension **separate** from the SQL [`PermissionMode`]
//! that gates queries. A path is readable only if it matches one of the allowed
//! glob patterns. The allowed set is seeded from the resolved `read_paths`
//! config and may be extended at runtime by in-session grants (the `/allow-dir`
//! command, or a "this session" choice at the approval prompt) which are never
//! written back to disk.
//!
//! [`PermissionMode`]: naque_core::PermissionMode

use std::path::{Path, PathBuf};

use glob::Pattern;

/// Outcome of authorizing a filesystem path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathAuth {
    /// The path matches an allowed glob; proceed.
    Allowed,
    /// The path is outside every allowed glob; the caller should prompt for a
    /// grant (interactive) or reject (non-interactive).
    NeedsGrant,
}

/// A compiled allow-glob paired with its original text (for display).
#[derive(Debug, Clone)]
struct AllowPattern {
    display: String,
    pattern: Pattern,
}

/// The set of glob patterns the agent is allowed to read, plus the base
/// directory that relative patterns and requested paths resolve against.
#[derive(Debug, Clone)]
pub struct FsAccess {
    base_dir: PathBuf,
    home: Option<PathBuf>,
    patterns: Vec<AllowPattern>,
}

impl FsAccess {
    /// Build from a base directory (project dir or CWD) and the resolved
    /// `read_paths` config globs. Invalid glob patterns in config are skipped.
    pub fn new(base_dir: impl Into<PathBuf>, read_paths: &[String]) -> Self {
        let base_dir = base_dir.into();
        // Canonicalize the base and home roots so the fixed prefix of each
        // expanded pattern matches a canonicalized requested path (important on
        // macOS where e.g. /tmp is a symlink to /private/tmp).
        let base_dir = std::fs::canonicalize(&base_dir).unwrap_or(base_dir);
        let home = dirs::home_dir().map(|h| std::fs::canonicalize(&h).unwrap_or(h));

        let mut access = FsAccess {
            base_dir,
            home,
            patterns: Vec::new(),
        };
        for raw in read_paths {
            // Config-sourced patterns: silently skip invalid ones.
            let _ = access.allow(raw);
        }
        access
    }

    /// Add a session grant. Returns an error if `raw` is not a valid glob so the
    /// caller (e.g. `/allow-dir`) can surface it. A plain (wildcard-free) path
    /// is expanded to also cover its subtree, so `~/sql` grants `~/sql/**`.
    pub fn allow(&mut self, raw: &str) -> Result<(), glob::PatternError> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Ok(());
        }
        let expanded = self.expand_path(raw);
        // Validate first so we don't register a half-set on error.
        let primary = Pattern::new(&expanded)?;
        let mut to_add = vec![AllowPattern {
            display: raw.to_string(),
            pattern: primary,
        }];
        // A wildcard-free entry names a file or directory; also cover its
        // subtree so granting a directory makes its contents readable.
        if !has_glob_meta(raw) {
            let subtree = format!("{}/**", expanded.trim_end_matches('/'));
            if let Ok(p) = Pattern::new(&subtree) {
                to_add.push(AllowPattern {
                    display: format!("{}/**", raw.trim_end_matches('/')),
                    pattern: p,
                });
            }
        }
        self.patterns.extend(to_add);
        Ok(())
    }

    /// Authorize a read of `path`. Resolves symlinks and `..` before matching,
    /// so a path that escapes the allowed roots (e.g. `~/sql/../../etc`) is
    /// reported as [`PathAuth::NeedsGrant`].
    pub fn authorize(&self, path: &str) -> PathAuth {
        let resolved = self.resolve(path);
        if self.patterns.iter().any(|p| p.pattern.matches_path(&resolved)) {
            PathAuth::Allowed
        } else {
            PathAuth::NeedsGrant
        }
    }

    /// The directory relative paths resolve against.
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// The currently-allowed globs, for display in denial messages.
    pub fn allowed_globs(&self) -> Vec<String> {
        self.patterns.iter().map(|p| p.display.clone()).collect()
    }

    /// Expand a raw pattern: `~` → home, relative → joined to `base_dir`,
    /// absolute → as-is. Returns a string suitable for `glob::Pattern::new`.
    pub fn expand_path(&self, raw: &str) -> String {
        if let Some(rest) = raw.strip_prefix("~/")
            && let Some(home) = &self.home
        {
            return home.join(rest).to_string_lossy().into_owned();
        }
        if raw == "~"
            && let Some(home) = &self.home
        {
            return home.to_string_lossy().into_owned();
        }
        let p = Path::new(raw);
        if p.is_absolute() {
            raw.to_string()
        } else {
            self.base_dir.join(raw).to_string_lossy().into_owned()
        }
    }

    /// Resolve a requested path to an absolute, symlink-free form for matching.
    fn resolve(&self, path: &str) -> PathBuf {
        let p = Path::new(path);
        let abs = if let Some(rest) = path.strip_prefix("~/") {
            self.home.clone().map(|h| h.join(rest)).unwrap_or_else(|| p.to_path_buf())
        } else if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.base_dir.join(p)
        };
        std::fs::canonicalize(&abs).unwrap_or(abs)
    }
}

/// True if `s` contains glob metacharacters (`*`, `?`, `[`).
fn has_glob_meta(s: &str) -> bool {
    s.contains(['*', '?', '['])
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, b"x").unwrap();
    }

    #[test]
    fn allows_subtree_of_plain_directory_grant() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        touch(&base.join("sql/q.sql"));
        let access = FsAccess::new(base, &["sql".to_string()]);
        assert_eq!(access.authorize("sql/q.sql"), PathAuth::Allowed);
        assert_eq!(access.authorize("sql"), PathAuth::Allowed);
    }

    #[test]
    fn glob_pattern_matches_nested() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        touch(&base.join("src/a/b.rs"));
        let access = FsAccess::new(base, &["src/**/*.rs".to_string()]);
        assert_eq!(access.authorize("src/a/b.rs"), PathAuth::Allowed);
    }

    #[test]
    fn denies_outside_allowed_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        touch(&base.join("sql/q.sql"));
        touch(&base.join("secrets/k.txt"));
        let access = FsAccess::new(base, &["sql/**".to_string()]);
        assert_eq!(access.authorize("secrets/k.txt"), PathAuth::NeedsGrant);
    }

    #[test]
    fn dotdot_escape_is_denied() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        touch(&base.join("sql/q.sql"));
        touch(&base.join("outside.txt"));
        let access = FsAccess::new(base, &["sql/**".to_string()]);
        // Lives in base, but reached via the allowed dir then escaping upward.
        assert_eq!(access.authorize("sql/../outside.txt"), PathAuth::NeedsGrant);
    }

    #[test]
    fn session_grant_extends_allowed_set() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        touch(&base.join("docs/readme.md"));
        let mut access = FsAccess::new(base, &[]);
        assert_eq!(access.authorize("docs/readme.md"), PathAuth::NeedsGrant);
        access.allow("docs").unwrap();
        assert_eq!(access.authorize("docs/readme.md"), PathAuth::Allowed);
    }

    #[test]
    fn empty_config_denies_everything() {
        let tmp = tempfile::tempdir().unwrap();
        let access = FsAccess::new(tmp.path(), &[]);
        assert_eq!(access.authorize("anything.txt"), PathAuth::NeedsGrant);
        assert!(access.allowed_globs().is_empty());
    }

    #[test]
    fn invalid_session_glob_reports_error() {
        let tmp = tempfile::tempdir().unwrap();
        let mut access = FsAccess::new(tmp.path(), &[]);
        assert!(access.allow("a[b").is_err());
    }
}
