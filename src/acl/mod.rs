//! Access-control rules declared in the vault's own frontmatter.
//!
//! A rule lives in the file it protects (D4), so renaming, moving, or deleting that
//! file carries or removes its rule with no bookkeeping. There is no rule-to-file
//! binding to maintain, no rename detection, and nothing to garbage-collect.
//!
//! Two properties are load-bearing and every change here must preserve them:
//!
//! 1. **Fail closed.** A path no rule names is denied (D9), and a rule block that will
//!    not parse denies everyone (D10). There is no code path that turns an error into
//!    access.
//! 2. **Rules never reach the browser.** `allow` and `deny` are stripped from the
//!    frontmatter that templates can see (SEC-6), so invitee addresses cannot leak into
//!    rendered HTML.

pub mod edit;
pub mod index;
pub mod resolver;

use serde_json::Value as JsonValue;

use crate::auth::{is_valid_email, normalize_email};

pub use index::AclIndex;
pub use resolver::{Decision, Resolution, Step};

/// The frontmatter keys that carry access rules.
pub const ALLOW_KEY: &str = "allow";
pub const DENY_KEY: &str = "deny";

/// Why a rule block could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AclError {
    /// The frontmatter key at fault.
    pub key: String,
    /// Human-readable explanation, suitable for `mdshelf check` output.
    pub message: String,
    /// 1-based line within the file, when it could be located.
    pub line: Option<usize>,
}

impl std::fmt::Display for AclError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

/// The access rules declared by one file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AclBlock {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    /// Non-empty when the block failed to parse. A poisoned block denies everyone (D10).
    pub errors: Vec<AclError>,
}

impl AclBlock {
    /// True when the block declares no rules at all and parsed cleanly.
    pub fn is_empty(&self) -> bool {
        self.allow.is_empty() && self.deny.is_empty() && self.errors.is_empty()
    }

    /// True when the block failed to parse, and therefore denies everyone.
    pub fn is_poisoned(&self) -> bool {
        !self.errors.is_empty()
    }

    /// How this block rules on `email`, if it mentions them at all.
    ///
    /// `deny` is checked first so that listing an address in both keys within a single
    /// block denies — the conservative reading of a contradiction.
    pub fn decide(&self, email: &str) -> Option<Decision> {
        if self.is_poisoned() {
            return Some(Decision::Deny);
        }
        if self.deny.iter().any(|entry| entry == email) {
            return Some(Decision::Deny);
        }
        if self.allow.iter().any(|entry| entry == email) {
            return Some(Decision::Allow);
        }
        None
    }

    /// Every address named by this block, for the derived index and diagnostics.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &str)> {
        self.allow
            .iter()
            .map(|email| ("allow", email.as_str()))
            .chain(self.deny.iter().map(|email| ("deny", email.as_str())))
    }
}

/// Parse the `allow` and `deny` keys out of already-parsed frontmatter.
///
/// `raw` is the original file text, used only to locate line numbers for error
/// reporting; parsing itself works from the structured value.
pub fn parse_acl(frontmatter: &JsonValue, raw: &str) -> AclBlock {
    let mut block = AclBlock::default();
    let Some(object) = frontmatter.as_object() else {
        return block;
    };

    for (key, target) in [(ALLOW_KEY, true), (DENY_KEY, false)] {
        let Some(value) = object.get(key) else {
            continue;
        };
        match parse_email_list(key, value, raw) {
            Ok(emails) => {
                if target {
                    block.allow = emails;
                } else {
                    block.deny = emails;
                }
            }
            Err(error) => block.errors.push(error),
        }
    }

    block
}

/// A rule value must be a list of valid addresses. Nothing else is accepted.
///
/// In particular a bare string is an error rather than a one-element list: the common
/// typo `allow: a@x.com, b@y.com` is a single string, and quietly reading it as one
/// malformed address would leave the author believing two people had been granted
/// access when neither had.
fn parse_email_list(key: &str, value: &JsonValue, raw: &str) -> Result<Vec<String>, AclError> {
    let line = find_key_line(raw, key);

    let JsonValue::Array(items) = value else {
        let found = describe_json(value);
        return Err(AclError {
            key: key.to_string(),
            message: format!(
                "`{key}` must be a list of email addresses, but found {found}. \
                 Write it as a YAML list:\n  {key}:\n    - someone@example.com"
            ),
            line,
        });
    };

    let mut emails = Vec::with_capacity(items.len());
    for item in items {
        let JsonValue::String(candidate) = item else {
            return Err(AclError {
                key: key.to_string(),
                message: format!(
                    "every entry under `{key}` must be an email address, but found {}",
                    describe_json(item)
                ),
                line,
            });
        };
        let normalized = normalize_email(candidate);
        if !is_valid_email(&normalized) {
            return Err(AclError {
                key: key.to_string(),
                message: format!("`{candidate}` is not a valid email address"),
                // Point at the offending entry rather than the key: in a list of thirty
                // addresses, "line 3" is not an answer to "which one is wrong?".
                line: find_entry_line(raw, candidate, line).or(line),
            });
        }
        if !emails.contains(&normalized) {
            emails.push(normalized);
        }
    }
    Ok(emails)
}

fn describe_json(value: &JsonValue) -> String {
    match value {
        JsonValue::Null => "an empty value".to_string(),
        JsonValue::Bool(_) => "a boolean".to_string(),
        JsonValue::Number(_) => "a number".to_string(),
        JsonValue::String(text) => format!("the string {text:?}"),
        JsonValue::Array(_) => "a list".to_string(),
        JsonValue::Object(_) => "a mapping".to_string(),
    }
}

/// Locate the 1-based line of a top-level frontmatter key, for error messages.
fn find_key_line(raw: &str, key: &str) -> Option<usize> {
    let mut in_frontmatter = false;
    for (offset, line) in raw.lines().enumerate() {
        let trimmed = line.trim_end();
        if trimmed == "---" {
            if in_frontmatter {
                return None; // Key not found before the block closed.
            }
            in_frontmatter = true;
            continue;
        }
        if in_frontmatter
            && line.starts_with(key)
            && line[key.len()..].trim_start().starts_with(':')
        {
            return Some(offset + 1);
        }
    }
    None
}

/// Find the line holding a specific list entry, searching from just after its key.
fn find_entry_line(raw: &str, needle: &str, key_line: Option<usize>) -> Option<usize> {
    let start = key_line.unwrap_or(0);
    raw.lines()
        .enumerate()
        .skip(start)
        .find(|(_, line)| line.contains(needle))
        .map(|(offset, _)| offset + 1)
}

/// Remove the rule keys from frontmatter before anything can render it (SEC-6).
///
/// Returns the value with `allow`/`deny` deleted. mdshelf exposes page frontmatter to
/// templates, so leaving these in would publish the invitee list to every reader.
pub fn strip_acl_keys(mut frontmatter: JsonValue) -> JsonValue {
    if let Some(object) = frontmatter.as_object_mut() {
        object.remove(ALLOW_KEY);
        object.remove(DENY_KEY);
    }
    frontmatter
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(yaml_body: &str) -> AclBlock {
        let raw = format!("---\n{yaml_body}---\n\n# Body\n");
        let matter = gray_matter::Matter::<gray_matter::engine::YAML>::new();
        let parsed: gray_matter::ParsedEntity<JsonValue> =
            matter.parse(&raw).expect("frontmatter parses");
        let value = parsed.data.unwrap_or(JsonValue::Object(Default::default()));
        parse_acl(&value, &raw)
    }

    #[test]
    fn parses_a_well_formed_block() {
        let block =
            parse("allow:\n  - ana@corp.com\n  - hr@corp.com\ndeny:\n  - intern@corp.com\n");
        assert_eq!(block.allow, vec!["ana@corp.com", "hr@corp.com"]);
        assert_eq!(block.deny, vec!["intern@corp.com"]);
        assert!(!block.is_poisoned());
    }

    #[test]
    fn parses_inline_yaml_lists() {
        let block = parse("allow: [ana@corp.com, bob@corp.com]\n");
        assert_eq!(block.allow, vec!["ana@corp.com", "bob@corp.com"]);
        assert!(!block.is_poisoned());
    }

    #[test]
    fn a_file_with_no_rules_is_empty_not_poisoned() {
        let block = parse("title: Something\n");
        assert!(block.is_empty());
        assert!(!block.is_poisoned());
    }

    #[test]
    fn a_bare_string_is_an_error_not_a_one_element_list() {
        // The motivating typo: this looks like two addresses but is one string.
        let block = parse("allow: ana@corp.com, bob@corp.com\n");
        assert!(block.is_poisoned(), "a bare string must poison the block");
        assert!(block.allow.is_empty(), "no address may be granted");
        let error = &block.errors[0];
        assert_eq!(error.key, "allow");
        assert!(
            error.message.contains("must be a list"),
            "{}",
            error.message
        );
        assert_eq!(error.line, Some(2), "the error should point at the key");
    }

    #[test]
    fn an_invalid_address_poisons_the_block() {
        let block = parse("allow:\n  - ana@corp\n");
        assert!(block.is_poisoned());
        assert!(block.errors[0].message.contains("ana@corp"));
        assert!(block.allow.is_empty());
    }

    #[test]
    fn an_invalid_address_is_reported_at_its_own_line() {
        // "which of these thirty addresses is wrong?" must have an answer.
        let block = parse("allow:\n  - ok@corp.com\n  - also-ok@corp.com\n  - broken@corp\n");
        assert!(block.is_poisoned());
        assert_eq!(
            block.errors[0].line,
            Some(5),
            "the error should point at the bad entry, not the `allow:` key"
        );
    }

    #[test]
    fn a_mapping_or_number_poisons_the_block() {
        assert!(parse("allow:\n  who: ana@corp.com\n").is_poisoned());
        assert!(parse("deny: 42\n").is_poisoned());
        assert!(parse("allow:\n  - 42\n").is_poisoned());
        assert!(parse("allow: true\n").is_poisoned());
    }

    #[test]
    fn addresses_are_normalized_and_deduplicated() {
        let block = parse("allow:\n  - Ana@Corp.COM\n  - ana@corp.com\n");
        assert_eq!(
            block.allow,
            vec!["ana@corp.com"],
            "case differences are the same person"
        );
    }

    #[test]
    fn a_poisoned_block_denies_everyone_including_listed_addresses() {
        let block = parse("allow:\n  - ana@corp.com\ndeny: oops\n");
        assert!(block.is_poisoned());
        assert_eq!(
            block.decide("ana@corp.com"),
            Some(Decision::Deny),
            "D10: a broken block must not grant anyone, even from its intact half"
        );
    }

    #[test]
    fn deny_wins_over_allow_inside_one_block() {
        let block = parse("allow:\n  - ana@corp.com\ndeny:\n  - ana@corp.com\n");
        assert_eq!(block.decide("ana@corp.com"), Some(Decision::Deny));
    }

    #[test]
    fn an_unlisted_address_is_undecided_by_the_block() {
        let block = parse("allow:\n  - ana@corp.com\n");
        assert_eq!(block.decide("bob@corp.com"), None);
    }

    #[test]
    fn strip_removes_both_rule_keys_and_nothing_else() {
        let value = json!({
            "title": "Compensation",
            "allow": ["ana@corp.com"],
            "deny": ["intern@corp.com"],
            "sidebar_order": 3,
        });
        let stripped = strip_acl_keys(value);
        assert!(stripped.get("allow").is_none());
        assert!(stripped.get("deny").is_none());
        assert_eq!(stripped.get("title").unwrap(), "Compensation");
        assert_eq!(stripped.get("sidebar_order").unwrap(), 3);
    }

    #[test]
    fn finds_line_numbers_for_both_keys() {
        let raw = "---\ntitle: X\nallow:\n  - a@b.com\ndeny:\n  - c@d.com\n---\n";
        assert_eq!(find_key_line(raw, "allow"), Some(3));
        assert_eq!(find_key_line(raw, "deny"), Some(5));
        assert_eq!(find_key_line(raw, "missing"), None);
    }

    #[test]
    fn does_not_mistake_body_text_for_a_key() {
        let raw = "---\ntitle: X\n---\n\nallow: this is prose in the body\n";
        assert_eq!(find_key_line(raw, "allow"), None);
    }
}
