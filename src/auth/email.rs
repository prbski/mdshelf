//! Email address validation.
//!
//! Deliberately conservative. These addresses are access-control subjects: an address
//! that looks valid here but does not match what Google returns in the `email` claim
//! grants nobody anything, and an address accepted loosely could mask a typo that the
//! vault owner believes granted access. Rejecting the ambiguous case is the safe error.

/// True when `raw` is an address mdshelf is willing to treat as an ACL subject.
///
/// Requires: ASCII only, exactly one `@`, a non-empty local part, and a domain of at
/// least two dot-separated labels each of which is non-empty and free of leading or
/// trailing hyphens.
pub fn is_valid_email(raw: &str) -> bool {
    if raw.is_empty() || raw.len() > 254 || !raw.is_ascii() {
        return false;
    }
    if raw
        .chars()
        .any(|c| c.is_ascii_whitespace() || c.is_ascii_control())
    {
        return false;
    }

    let mut parts = raw.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };

    if local.is_empty() || local.len() > 64 || local.starts_with('.') || local.ends_with('.') {
        return false;
    }
    if local.contains("..") {
        return false;
    }
    if !local
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+-/=?^_`{|}~.".contains(c))
    {
        return false;
    }

    is_valid_domain(domain)
}

fn is_valid_domain(domain: &str) -> bool {
    if domain.is_empty() || domain.len() > 253 {
        return false;
    }
    let labels: Vec<&str> = domain.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    labels.iter().all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

/// Normalize an address for comparison. Google returns the local part case-sensitively
/// in principle but treats it case-insensitively in practice; lowercasing both the
/// stored rule and the verified claim keeps `Ana@corp.com` in a vault matching
/// `ana@corp.com` from Google.
pub fn normalize_email(raw: &str) -> String {
    raw.trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_addresses() {
        assert!(is_valid_email("ana@corp.com"));
        assert!(is_valid_email("hr-lead@corp.co.uk"));
        assert!(is_valid_email("first.last+tag@sub.example.org"));
    }

    #[test]
    fn rejects_addresses_without_a_full_domain() {
        // The interview called this out explicitly: "ana@corp" must not validate.
        assert!(!is_valid_email("ana@corp"));
        assert!(!is_valid_email("ana@"));
        assert!(!is_valid_email("@corp.com"));
    }

    #[test]
    fn rejects_malformed_shapes() {
        assert!(!is_valid_email(""));
        assert!(!is_valid_email("ana"));
        assert!(!is_valid_email("a@b@c.com"));
        assert!(!is_valid_email("ana @corp.com"));
        assert!(!is_valid_email("ana@corp .com"));
        assert!(!is_valid_email("ana@corp..com"));
        assert!(!is_valid_email("ana@-corp.com"));
        assert!(!is_valid_email("ana@corp-.com"));
        assert!(!is_valid_email(".ana@corp.com"));
        assert!(!is_valid_email("ana.@corp.com"));
        assert!(!is_valid_email("an..a@corp.com"));
    }

    #[test]
    fn rejects_non_ascii() {
        assert!(!is_valid_email("aña@corp.com"));
    }

    #[test]
    fn normalizes_case_and_whitespace() {
        assert_eq!(normalize_email("  Ana@Corp.COM "), "ana@corp.com");
    }
}
