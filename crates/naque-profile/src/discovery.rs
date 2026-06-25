//! Upward `naque.toml` discovery: walk from a start directory to the root.

use std::path::{Path, PathBuf};

/// Walk up from `start` (inclusive) toward the filesystem root.
/// Returns the path of the first `naque.toml` found, or `None`.
pub fn find_naque_toml(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        let candidate = current.join("naque.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
        if !current.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn finds_naque_toml_in_ancestor() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Create a/b/c directories; place naque.toml in a
        let a = root.join("a");
        let b = a.join("b");
        let c = b.join("c");
        fs::create_dir_all(&c).unwrap();
        let toml_path = a.join("naque.toml");
        fs::write(&toml_path, "project = \"test\"\n").unwrap();

        let found = find_naque_toml(&c).unwrap();
        assert_eq!(found, toml_path);
    }

    #[test]
    fn finds_naque_toml_in_start_itself() {
        let tmp = tempfile::tempdir().unwrap();
        let toml_path = tmp.path().join("naque.toml");
        std::fs::write(&toml_path, "").unwrap();

        let found = find_naque_toml(tmp.path()).unwrap();
        assert_eq!(found, toml_path);
    }

    #[test]
    fn returns_none_when_no_toml_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("x").join("y");
        fs::create_dir_all(&nested).unwrap();

        // No naque.toml anywhere in this tree (or above, assuming real fs root
        // has no naque.toml — tested against the temp dir which is isolated).
        // We start from nested but there is no naque.toml. We can't guarantee
        // the real root has none, so we just verify the function returns None
        // when walking from a path where we know there is none by placing the
        // start inside the temp dir (which has no naque.toml).
        assert!(find_naque_toml(&nested).is_none());
    }
}
