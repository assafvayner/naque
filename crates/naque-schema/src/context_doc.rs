//! The `context.md` document: a profile title plus three sections —
//! `## Schema` (mechanical), `## Overview` (LLM), `## Notes` (user-owned).
//!
//! `/save` regenerates Schema + Overview but must preserve the user's Notes;
//! `extract_notes` pulls the existing Notes out of a prior document so
//! `assemble` can put them back.

/// Assemble a full `context.md` from its parts.
pub fn assemble(profile: &str, schema_md: &str, overview: &str, notes: &str) -> String {
    format!(
        "# {profile} — context\n\n## Schema\n\n{schema}\n\n## Overview\n\n{overview}\n\n## Notes\n\n{notes}\n",
        profile = profile,
        schema = schema_md.trim(),
        overview = overview.trim(),
        notes = notes.trim(),
    )
}

/// Extract the body of the `## Notes` section from an existing document.
/// Returns "" if there is no Notes section. The returned text excludes the
/// `## Notes` heading and surrounding blank lines.
pub fn extract_notes(doc: &str) -> String {
    let mut lines = doc.lines();
    let mut collecting = false;
    let mut out: Vec<&str> = Vec::new();
    for line in &mut lines {
        let is_heading = line.trim_start().starts_with("## ");
        if collecting {
            if is_heading {
                break; // next section ends Notes
            }
            out.push(line);
        } else if is_heading && line.trim() == "## Notes" {
            collecting = true;
        }
    }
    out.join("\n").trim().to_string()
}

/// Append a note to a document's Notes section (preserving Schema/Overview),
/// returning the new document. If `doc` has no Notes section, one is created.
pub fn append_note(doc: &str, note: &str) -> String {
    let existing = extract_notes(doc);
    let merged = if existing.is_empty() {
        note.trim().to_string()
    } else {
        format!("{existing}\n{}", note.trim())
    };
    let head = match doc.find("## Notes") {
        Some(i) => doc[..i].trim_end().to_string(),
        None => doc.trim_end().to_string(),
    };
    format!("{head}\n\n## Notes\n\n{merged}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assemble_has_three_sections() {
        let d = assemble("shop", "### t", "an overview", "my note");
        assert!(d.starts_with("# shop — context"));
        assert!(d.contains("## Schema"));
        assert!(d.contains("## Overview"));
        assert!(d.contains("## Notes"));
        assert!(d.contains("my note"));
    }

    #[test]
    fn extract_notes_pulls_body_only() {
        let d = assemble("shop", "### t", "ov", "line one\nline two");
        assert_eq!(extract_notes(&d), "line one\nline two");
    }

    #[test]
    fn regenerate_preserves_notes() {
        let original = assemble("shop", "### old", "old overview", "user wrote this");
        let notes = extract_notes(&original);
        let regenerated = assemble("shop", "### new", "new overview", &notes);
        assert!(regenerated.contains("### new"));
        assert!(regenerated.contains("new overview"));
        assert!(regenerated.contains("user wrote this"));
    }

    #[test]
    fn append_note_keeps_prior_notes_and_sections() {
        let d = assemble("shop", "### t", "ov", "first");
        let d2 = append_note(&d, "second");
        assert!(d2.contains("first"));
        assert!(d2.contains("second"));
        assert!(d2.contains("## Schema"));
    }

    #[test]
    fn extract_notes_empty_when_absent() {
        assert_eq!(extract_notes("# x\n\n## Schema\n\nbody"), "");
    }
}
