//! Editing `allow` lists in place.
//!
//! This is the only code in mdshelf that writes to a user's vault (D32), and it is
//! editing a file the user cares about — often one under version control or a sync
//! client. It therefore works as a careful text edit rather than a YAML round-trip:
//! comments, key order, quoting style, and formatting elsewhere in the file are left
//! exactly as they were, because a command that reformats somebody's notes as a side
//! effect of adding one address is not a command they will run twice.

use anyhow::{Result, bail};

const FRONTMATTER_FENCE: &str = "---";

/// Add `email` to the file's `allow` list, returning the new text.
///
/// Returns `Ok(None)` when the address is already listed, so the caller can report
/// "already granted" rather than writing an identical file.
pub fn add_to_allow_list(source: &str, email: &str) -> Result<Option<String>> {
    let line_ending = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let lines: Vec<&str> = source.split_inclusive('\n').collect();

    let Some((open, close)) = frontmatter_bounds(&lines) else {
        // Adding frontmatter to a file that has none would change how mdshelf reads its
        // title and layout, so it is the author's call, not ours.
        bail!(
            "this file has no frontmatter block. Add one first:\n  ---\n  allow:\n    - {email}\n  ---"
        );
    };

    let allow_line = (open + 1..close).find(|&index| is_key(lines[index], "allow"));

    let Some(allow_index) = allow_line else {
        // No `allow` yet: insert a fresh block just inside the closing fence.
        let mut out = String::with_capacity(source.len() + email.len() + 16);
        for line in &lines[..close] {
            out.push_str(line);
        }
        ensure_trailing_newline(&mut out, line_ending);
        out.push_str("allow:");
        out.push_str(line_ending);
        out.push_str("  - ");
        out.push_str(email);
        out.push_str(line_ending);
        for line in &lines[close..] {
            out.push_str(line);
        }
        return Ok(Some(out));
    };

    let value = lines[allow_index]
        .split_once(':')
        .map(|(_, rest)| rest.trim())
        .unwrap_or_default();

    // An inline list (`allow: [a@x.com]`) is rewritten in place, keeping it inline.
    if let Some(inner) = value
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    {
        let existing: Vec<&str> = inner
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .collect();
        if existing
            .iter()
            .any(|entry| entry.trim_matches('"') == email)
        {
            return Ok(None);
        }
        let mut entries: Vec<String> = existing.iter().map(|s| s.to_string()).collect();
        entries.push(email.to_string());
        let rebuilt = format!("allow: [{}]", entries.join(", "));

        let mut out = String::with_capacity(source.len() + email.len() + 4);
        for (index, line) in lines.iter().enumerate() {
            if index == allow_index {
                out.push_str(&rebuilt);
                out.push_str(line_ending);
            } else {
                out.push_str(line);
            }
        }
        return Ok(Some(out));
    }

    if !value.is_empty() {
        // A scalar value here is the malformed shape D10 rejects; rewriting it would be
        // guessing at what the author meant.
        bail!(
            "`allow` in this file is not a list (found `{value}`). Fix it by hand:\n  \
             allow:\n    - someone@example.com"
        );
    }

    // A block list: find its extent and append, matching the existing indentation.
    let mut last_entry = allow_index;
    let mut indent = "  - ".to_string();
    for (index, line) in lines.iter().enumerate().take(close).skip(allow_index + 1) {
        let trimmed = line.trim_start();
        if trimmed.starts_with('-') {
            if trimmed
                .trim_end()
                .trim_start_matches('-')
                .trim()
                .trim_matches('"')
                == email
            {
                return Ok(None);
            }
            let leading = &line[..line.len() - trimmed.len()];
            indent = format!("{leading}- ");
            last_entry = index;
        } else if !trimmed.trim().is_empty() {
            break;
        }
    }

    let mut out = String::with_capacity(source.len() + email.len() + indent.len() + 2);
    for (index, line) in lines.iter().enumerate() {
        out.push_str(line);
        if index == last_entry {
            ensure_trailing_newline(&mut out, line_ending);
            out.push_str(&indent);
            out.push_str(email);
            out.push_str(line_ending);
        }
    }
    Ok(Some(out))
}

fn ensure_trailing_newline(buffer: &mut String, line_ending: &str) {
    if !buffer.ends_with('\n') {
        buffer.push_str(line_ending);
    }
}

/// True when a line declares `key:` at the top level of the frontmatter.
fn is_key(line: &str, key: &str) -> bool {
    if line.starts_with(char::is_whitespace) {
        return false;
    }
    line.strip_prefix(key)
        .is_some_and(|rest| rest.trim_start().starts_with(':'))
}

/// Indices of the opening and closing `---` fences.
fn frontmatter_bounds(lines: &[&str]) -> Option<(usize, usize)> {
    let first = lines
        .iter()
        .position(|line| line.trim_end() == FRONTMATTER_FENCE)?;
    if lines[..first].iter().any(|line| !line.trim().is_empty()) {
        return None; // Frontmatter must open the file.
    }
    let close = lines
        .iter()
        .enumerate()
        .skip(first + 1)
        .find(|(_, line)| line.trim_end() == FRONTMATTER_FENCE)
        .map(|(index, _)| index)?;
    Some((first, close))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_to_an_existing_block_list() {
        let source = "---\ntitle: Comp\nallow:\n  - ana@corp.com\n---\n\n# Body\n";
        let updated = add_to_allow_list(source, "bob@corp.com").unwrap().unwrap();
        assert_eq!(
            updated,
            "---\ntitle: Comp\nallow:\n  - ana@corp.com\n  - bob@corp.com\n---\n\n# Body\n"
        );
    }

    #[test]
    fn adds_an_allow_block_when_there_is_none() {
        let source = "---\ntitle: Comp\n---\n\n# Body\n";
        let updated = add_to_allow_list(source, "ana@corp.com").unwrap().unwrap();
        assert_eq!(
            updated,
            "---\ntitle: Comp\nallow:\n  - ana@corp.com\n---\n\n# Body\n"
        );
    }

    #[test]
    fn extends_an_inline_list_without_reflowing_it() {
        let source = "---\nallow: [ana@corp.com]\n---\n";
        let updated = add_to_allow_list(source, "bob@corp.com").unwrap().unwrap();
        assert_eq!(updated, "---\nallow: [ana@corp.com, bob@corp.com]\n---\n");
    }

    #[test]
    fn reports_an_address_that_is_already_listed() {
        let source = "---\nallow:\n  - ana@corp.com\n---\n";
        assert!(add_to_allow_list(source, "ana@corp.com").unwrap().is_none());

        let inline = "---\nallow: [ana@corp.com]\n---\n";
        assert!(add_to_allow_list(inline, "ana@corp.com").unwrap().is_none());
    }

    #[test]
    fn preserves_comments_key_order_and_other_content() {
        let source = "---\n# who owns this page\nowner: hr\nallow:\n  - ana@corp.com\ndeny:\n  - x@corp.com\nsidebar_order: 3\n---\n\nBody stays.\n";
        let updated = add_to_allow_list(source, "bob@corp.com").unwrap().unwrap();

        assert!(updated.contains("# who owns this page"));
        assert!(updated.contains("owner: hr"));
        assert!(updated.contains("deny:\n  - x@corp.com"));
        assert!(updated.contains("sidebar_order: 3"));
        assert!(updated.contains("Body stays."));
        assert!(updated.contains("  - ana@corp.com\n  - bob@corp.com\n"));
    }

    #[test]
    fn matches_the_existing_indentation() {
        let source = "---\nallow:\n    - ana@corp.com\n---\n";
        let updated = add_to_allow_list(source, "bob@corp.com").unwrap().unwrap();
        assert!(updated.contains("    - ana@corp.com\n    - bob@corp.com\n"));
    }

    #[test]
    fn preserves_windows_line_endings() {
        let source = "---\r\ntitle: Comp\r\nallow:\r\n  - ana@corp.com\r\n---\r\n";
        let updated = add_to_allow_list(source, "bob@corp.com").unwrap().unwrap();
        assert!(updated.contains("  - bob@corp.com\r\n"));
        assert!(!updated.contains("  - bob@corp.com\n\r"));
    }

    #[test]
    fn refuses_a_file_without_frontmatter() {
        let error = add_to_allow_list("# Just a heading\n", "ana@corp.com").unwrap_err();
        assert!(error.to_string().contains("no frontmatter"));
    }

    #[test]
    fn refuses_to_guess_at_a_malformed_allow_value() {
        // Exactly the D10 shape: rewriting it would be inventing the author's intent.
        let error = add_to_allow_list(
            "---\nallow: ana@corp.com, bob@corp.com\n---\n",
            "c@corp.com",
        )
        .unwrap_err();
        assert!(error.to_string().contains("not a list"));
    }

    #[test]
    fn does_not_mistake_a_body_line_for_the_key() {
        let source = "---\ntitle: X\n---\n\nallow: this is prose\n";
        let updated = add_to_allow_list(source, "ana@corp.com").unwrap().unwrap();
        assert!(updated.contains("allow:\n  - ana@corp.com\n---"));
        assert!(updated.contains("allow: this is prose"));
    }

    #[test]
    fn does_not_mistake_a_nested_key_for_the_top_level_one() {
        let source = "---\nmeta:\n  allow: nested\n---\n";
        let updated = add_to_allow_list(source, "ana@corp.com").unwrap().unwrap();
        assert!(updated.contains("  allow: nested"));
        assert!(updated.contains("allow:\n  - ana@corp.com\n"));
    }
}
