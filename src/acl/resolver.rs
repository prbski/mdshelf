//! Resolving an effective decision for one (path, email) pair.
//!
//! The algorithm is D6: rules inherit downward, the most specific level that names the
//! address wins, and an explicit `deny` at that level overrides an inherited `allow`.
//! When no level names the address at all, the answer is Deny (D9).
//!
//! Resolution produces a full trace, not just a verdict. Most-specific-wins is exactly
//! the kind of rule that is easy to write and hard to debug, so `mdshelf acl explain`
//! can show which rule decided and why (US-14).

use std::fmt;

/// The outcome of resolving a path for an address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
}

impl Decision {
    pub fn is_allowed(self) -> bool {
        matches!(self, Decision::Allow)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Allow => "allow",
            Decision::Deny => "deny",
        }
    }
}

impl fmt::Display for Decision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Which level of the hierarchy a rule came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    File,
    Folder,
    Site,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::File => "file",
            Level::Folder => "folder",
            Level::Site => "site",
        }
    }
}

impl fmt::Display for Level {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One rung of the resolution ladder, recorded whether or not it matched.
#[derive(Debug, Clone)]
pub struct Step {
    pub level: Level,
    /// The file the rule was read from, relative to the site root.
    pub source: String,
    /// What this level said about the address, if anything.
    pub decision: Option<Decision>,
    /// Set when this level's rule block failed to parse.
    pub poisoned: bool,
    /// True for the level that decided the outcome.
    pub decisive: bool,
}

/// A verdict plus the reasoning that produced it.
#[derive(Debug, Clone)]
pub struct Resolution {
    pub decision: Decision,
    pub steps: Vec<Step>,
    /// Set when the verdict came from the fail-closed default rather than a rule.
    pub defaulted: bool,
    /// Set when a malformed rule block forced the verdict.
    pub poisoned_source: Option<String>,
}

impl Resolution {
    pub fn is_allowed(&self) -> bool {
        self.decision.is_allowed()
    }

    /// The rule that decided, if a rule did.
    pub fn decisive_step(&self) -> Option<&Step> {
        self.steps.iter().find(|step| step.decisive)
    }

    /// A one-line explanation of the verdict.
    pub fn reason(&self) -> String {
        if let Some(source) = self.poisoned_source.as_deref() {
            return format!("denied: the rule block in {source} could not be parsed");
        }
        if self.defaulted {
            return "denied: no rule at any level names this address (fail-closed default)"
                .to_string();
        }
        match self.decisive_step() {
            Some(step) => format!(
                "{}: {} rule in {}",
                step.decision.unwrap_or(Decision::Deny),
                step.level,
                step.source
            ),
            None => "denied".to_string(),
        }
    }
}

/// A rule block paired with where it came from, ordered most specific first.
pub struct Candidate<'a> {
    pub level: Level,
    pub source: String,
    pub block: &'a super::AclBlock,
}

/// Resolve `email` against an ordered chain of rule blocks.
///
/// `candidates` must run from most specific to least: the file itself, then each
/// ancestor folder from nearest to furthest, then the site root.
pub fn resolve<'a>(candidates: impl IntoIterator<Item = Candidate<'a>>, email: &str) -> Resolution {
    let mut steps = Vec::new();
    let mut verdict: Option<Decision> = None;
    let mut poisoned_source = None;

    for candidate in candidates {
        let poisoned = candidate.block.is_poisoned();
        let decision = candidate.block.decide(email);

        // A malformed block anywhere in the chain denies, without consulting the rest.
        // Falling back to an ancestor's rules here would mean a typo silently *widened*
        // access to whatever the parent folder grants (D10).
        if poisoned && verdict.is_none() {
            poisoned_source = Some(candidate.source.clone());
            verdict = Some(Decision::Deny);
            steps.push(Step {
                level: candidate.level,
                source: candidate.source,
                decision: Some(Decision::Deny),
                poisoned: true,
                decisive: true,
            });
            break;
        }

        let decisive = verdict.is_none() && decision.is_some();
        if decisive {
            verdict = decision;
        }
        steps.push(Step {
            level: candidate.level,
            source: candidate.source,
            decision,
            poisoned,
            decisive,
        });

        if verdict.is_some() {
            break;
        }
    }

    match verdict {
        Some(decision) => Resolution {
            decision,
            steps,
            defaulted: false,
            poisoned_source,
        },
        // D9: nothing named this address, so nothing granted it anything.
        None => Resolution {
            decision: Decision::Deny,
            steps,
            defaulted: true,
            poisoned_source: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acl::AclBlock;

    fn block(allow: &[&str], deny: &[&str]) -> AclBlock {
        AclBlock {
            allow: allow.iter().map(|s| s.to_string()).collect(),
            deny: deny.iter().map(|s| s.to_string()).collect(),
            errors: Vec::new(),
        }
    }

    fn poisoned() -> AclBlock {
        AclBlock {
            allow: Vec::new(),
            deny: Vec::new(),
            errors: vec![crate::acl::AclError {
                key: "allow".into(),
                message: "broken".into(),
                line: Some(2),
            }],
        }
    }

    fn chain<'a>(levels: &'a [(Level, &'a str, &'a AclBlock)]) -> Vec<Candidate<'a>> {
        levels
            .iter()
            .map(|(level, source, block)| Candidate {
                level: *level,
                source: source.to_string(),
                block,
            })
            .collect()
    }

    #[test]
    fn file_deny_overrides_inherited_folder_allow() {
        let file = block(&[], &["intern@corp.com"]);
        let folder = block(&["intern@corp.com"], &[]);
        let resolution = resolve(
            chain(&[
                (Level::File, "hr/comp.md", &file),
                (Level::Folder, "hr/index.md", &folder),
            ]),
            "intern@corp.com",
        );
        assert_eq!(resolution.decision, Decision::Deny);
        assert_eq!(resolution.decisive_step().unwrap().level, Level::File);
    }

    #[test]
    fn file_allow_overrides_inherited_folder_deny() {
        let file = block(&["ana@corp.com"], &[]);
        let folder = block(&[], &["ana@corp.com"]);
        let resolution = resolve(
            chain(&[
                (Level::File, "hr/comp.md", &file),
                (Level::Folder, "hr/index.md", &folder),
            ]),
            "ana@corp.com",
        );
        assert_eq!(resolution.decision, Decision::Allow);
        assert_eq!(resolution.decisive_step().unwrap().level, Level::File);
    }

    #[test]
    fn a_nearer_folder_overrides_a_further_one() {
        let empty = AclBlock::default();
        let near = block(&[], &["ana@corp.com"]);
        let far = block(&["ana@corp.com"], &[]);
        let resolution = resolve(
            chain(&[
                (Level::File, "hr/pay/comp.md", &empty),
                (Level::Folder, "hr/pay/index.md", &near),
                (Level::Folder, "hr/index.md", &far),
            ]),
            "ana@corp.com",
        );
        assert_eq!(resolution.decision, Decision::Deny);
        assert_eq!(
            resolution.decisive_step().unwrap().source,
            "hr/pay/index.md"
        );
    }

    #[test]
    fn any_folder_rule_overrides_the_site_rule() {
        let empty = AclBlock::default();
        let folder = block(&[], &["ana@corp.com"]);
        let site = block(&["ana@corp.com"], &[]);
        let resolution = resolve(
            chain(&[
                (Level::File, "hr/comp.md", &empty),
                (Level::Folder, "hr/index.md", &folder),
                (Level::Site, "index.md", &site),
            ]),
            "ana@corp.com",
        );
        assert_eq!(resolution.decision, Decision::Deny);
        assert_eq!(resolution.decisive_step().unwrap().level, Level::Folder);
    }

    #[test]
    fn a_site_rule_covers_a_file_with_no_nearer_rule() {
        let empty = AclBlock::default();
        let site = block(&["ana@corp.com"], &[]);
        let resolution = resolve(
            chain(&[
                (Level::File, "notes/idea.md", &empty),
                (Level::Folder, "notes/index.md", &empty),
                (Level::Site, "index.md", &site),
            ]),
            "ana@corp.com",
        );
        assert_eq!(resolution.decision, Decision::Allow);
        assert_eq!(resolution.decisive_step().unwrap().level, Level::Site);
        assert!(!resolution.defaulted);
    }

    #[test]
    fn an_address_named_nowhere_is_denied_by_default() {
        let empty = AclBlock::default();
        let site = block(&["ana@corp.com"], &[]);
        let resolution = resolve(
            chain(&[
                (Level::File, "notes/idea.md", &empty),
                (Level::Site, "index.md", &site),
            ]),
            "stranger@elsewhere.com",
        );
        assert_eq!(resolution.decision, Decision::Deny);
        assert!(resolution.defaulted, "D9: the default must be deny");
        assert!(resolution.reason().contains("fail-closed"));
    }

    #[test]
    fn an_empty_chain_denies() {
        let resolution = resolve(Vec::new(), "anyone@corp.com");
        assert_eq!(resolution.decision, Decision::Deny);
        assert!(resolution.defaulted);
    }

    #[test]
    fn a_poisoned_file_denies_regardless_of_ancestor_grants() {
        let broken = poisoned();
        let site = block(&["ana@corp.com"], &[]);
        let resolution = resolve(
            chain(&[
                (Level::File, "hr/comp.md", &broken),
                (Level::Site, "index.md", &site),
            ]),
            "ana@corp.com",
        );
        assert_eq!(resolution.decision, Decision::Deny);
        assert_eq!(
            resolution.poisoned_source.as_deref(),
            Some("hr/comp.md"),
            "the report must name the file that needs fixing"
        );
        assert!(resolution.reason().contains("could not be parsed"));
    }

    #[test]
    fn a_poisoned_folder_denies_its_subtree() {
        let empty = AclBlock::default();
        let broken = poisoned();
        let site = block(&["ana@corp.com"], &[]);
        let resolution = resolve(
            chain(&[
                (Level::File, "hr/comp.md", &empty),
                (Level::Folder, "hr/index.md", &broken),
                (Level::Site, "index.md", &site),
            ]),
            "ana@corp.com",
        );
        assert_eq!(resolution.decision, Decision::Deny);
        assert_eq!(resolution.poisoned_source.as_deref(), Some("hr/index.md"));
    }

    #[test]
    fn a_decision_taken_before_a_poisoned_ancestor_still_stands() {
        // The file grants access; a broken rule further up must not retroactively
        // revoke it, because the more specific rule already decided.
        let file = block(&["ana@corp.com"], &[]);
        let broken = poisoned();
        let resolution = resolve(
            chain(&[
                (Level::File, "hr/comp.md", &file),
                (Level::Folder, "hr/index.md", &broken),
            ]),
            "ana@corp.com",
        );
        assert_eq!(resolution.decision, Decision::Allow);
        assert!(resolution.poisoned_source.is_none());
    }

    #[test]
    fn the_trace_records_levels_that_did_not_match() {
        let empty = AclBlock::default();
        let site = block(&["ana@corp.com"], &[]);
        let resolution = resolve(
            chain(&[
                (Level::File, "notes/idea.md", &empty),
                (Level::Folder, "notes/index.md", &empty),
                (Level::Site, "index.md", &site),
            ]),
            "ana@corp.com",
        );
        assert_eq!(resolution.steps.len(), 3);
        assert!(resolution.steps[0].decision.is_none());
        assert!(resolution.steps[1].decision.is_none());
        assert!(resolution.steps[2].decisive);
    }
}
