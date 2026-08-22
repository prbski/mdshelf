//! The per-site ACL index: rule blocks arranged so a chain can be assembled for any path.
//!
//! Built from the vault on load and rebuilt by the watcher, so an edit takes effect on
//! the next request (D3). It is derived state only — the markdown is the source of truth.

use std::collections::BTreeMap;
use std::path::Path;

use crate::auth::crypto::signature_digest;

use super::resolver::{Candidate, Level, Resolution, resolve};
use super::{AclBlock, AclError};

/// True when `rel_path` is a folder's index file, and therefore governs that folder
/// and everything beneath it (D7/D8).
///
/// Delegates to mdshelf's own notion of an index page rather than re-deriving it. An
/// independent definition drifted, with real consequences: it counted `readme.md` and
/// `index.markdown` as folder indexes when mdshelf serves both as ordinary pages. An
/// author granting access to one such file was silently granting the whole folder —
/// a `deny` on a sibling would not save them, because the widened rule sits at folder
/// level.
///
/// US-10 says as much: README is an index "where mdshelf already treats it as one".
/// Asking mdshelf is the only way to keep that true.
pub fn is_index_file(rel_path: &Path) -> bool {
    crate::content::page::url_path_from_rel(rel_path).1
}

/// The folder a relative path sits in, as a `/`-joined string ("" for the site root).
pub fn parent_folder(rel_path: &Path) -> String {
    rel_path
        .parent()
        .map(|parent| normalize_folder(&parent.to_string_lossy()))
        .unwrap_or_default()
}

fn normalize_folder(raw: &str) -> String {
    raw.replace('\\', "/").trim_matches('/').to_string()
}

/// A rule block plus the file it came from.
#[derive(Debug, Clone)]
struct Source {
    block: AclBlock,
    /// Path of the declaring file, relative to the site root.
    file: String,
}

/// Rule blocks for one site, arranged for lookup.
#[derive(Debug, Clone, Default)]
pub struct AclIndex {
    /// Folder path ("" is the site root) -> rules from that folder's index file.
    folders: BTreeMap<String, Source>,
    /// Relative file path -> rules from that non-index file.
    files: BTreeMap<String, Source>,
    /// Whether the site declares any rule at all.
    has_rules: bool,
}

impl AclIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the rules declared by one file.
    ///
    /// An index file's rules are filed under its folder, because they govern that folder
    /// and everything beneath it, including the index page itself (D8).
    pub fn insert(&mut self, rel_path: &Path, block: AclBlock) {
        if block.is_empty() {
            return;
        }
        self.has_rules = true;
        let file = normalize_folder(&rel_path.to_string_lossy());
        let source = Source { block, file };

        if is_index_file(rel_path) {
            self.folders.insert(parent_folder(rel_path), source);
        } else {
            self.files
                .insert(normalize_folder(&rel_path.to_string_lossy()), source);
        }
    }

    /// True when the site declares no rules at all.
    ///
    /// Such a site is served exactly as it was before auth existed, which is what makes
    /// `--auth google` additive rather than a behaviour change for existing vaults.
    pub fn is_empty(&self) -> bool {
        !self.has_rules
    }

    /// Every malformed rule block, for `mdshelf check` and `acl doctor`.
    pub fn poisoned(&self) -> Vec<(&str, &AclError)> {
        self.folders
            .values()
            .chain(self.files.values())
            .filter(|source| source.block.is_poisoned())
            .flat_map(|source| {
                source
                    .block
                    .errors
                    .iter()
                    .map(move |error| (source.file.as_str(), error))
            })
            .collect()
    }

    /// Every address named anywhere in the site.
    pub fn known_emails(&self) -> Vec<String> {
        let mut emails: Vec<String> = self
            .folders
            .values()
            .chain(self.files.values())
            .flat_map(|source| source.block.entries().map(|(_, email)| email.to_string()))
            .collect();
        emails.sort();
        emails.dedup();
        emails
    }

    /// Folders that declare rules, used by diagnostics.
    pub fn rule_folders(&self) -> impl Iterator<Item = &str> {
        self.folders.keys().map(String::as_str)
    }

    /// Rows for the derived SQLite index (D13).
    pub fn rows(&self) -> Vec<crate::auth::store::RuleRow> {
        let mut rows = Vec::new();
        for (folder, source) in &self.folders {
            let level = if folder.is_empty() { "site" } else { "folder" };
            for (effect, email) in source.block.entries() {
                rows.push(crate::auth::store::RuleRow {
                    path: folder.clone(),
                    level: level.to_string(),
                    effect: effect.to_string(),
                    email: email.to_string(),
                });
            }
        }
        for (path, source) in &self.files {
            for (effect, email) in source.block.entries() {
                rows.push(crate::auth::store::RuleRow {
                    path: path.clone(),
                    level: "file".to_string(),
                    effect: effect.to_string(),
                    email: email.to_string(),
                });
            }
        }
        rows
    }

    /// Build the ordered chain of rule blocks that applies to `rel_path`.
    ///
    /// Ordered most specific first: the file's own rules (for a non-index file), then
    /// each ancestor folder from nearest to furthest, then the site root.
    fn chain(&self, rel_path: &Path) -> Vec<Candidate<'_>> {
        let mut candidates = Vec::new();
        let normalized = normalize_folder(&rel_path.to_string_lossy());

        // An index file has no file-level rules of its own: what it declares is the
        // folder rule, which is picked up by the folder walk below (D8).
        if !is_index_file(rel_path)
            && let Some(source) = self.files.get(&normalized)
        {
            candidates.push(Candidate {
                level: Level::File,
                source: source.file.clone(),
                block: &source.block,
            });
        }

        let mut folder = parent_folder(rel_path);
        loop {
            if let Some(source) = self.folders.get(&folder) {
                candidates.push(Candidate {
                    level: if folder.is_empty() {
                        Level::Site
                    } else {
                        Level::Folder
                    },
                    source: source.file.clone(),
                    block: &source.block,
                });
            }
            if folder.is_empty() {
                break;
            }
            folder = match folder.rsplit_once('/') {
                Some((parent, _)) => parent.to_string(),
                None => String::new(),
            };
        }

        candidates
    }

    /// Resolve `rel_path` for `email`, with a full trace.
    pub fn resolve(&self, rel_path: &Path, email: &str) -> Resolution {
        resolve(self.chain(rel_path), email)
    }

    /// Whether `email` may read `rel_path`.
    pub fn allows(&self, rel_path: &Path, email: &str) -> bool {
        self.resolve(rel_path, email).is_allowed()
    }

    /// A stable digest of the rules that bear on `email` anywhere in this site.
    ///
    /// Two viewers with the same signature see exactly the same site, so they can share
    /// one cached navigation tree and search index (D12).
    pub fn signature(&self, email: &str) -> String {
        let mut parts: Vec<String> = Vec::new();
        for (folder, source) in &self.folders {
            if let Some(decision) = source.block.decide(email) {
                parts.push(format!("f:{folder}:{}", decision.as_str()));
            } else if source.block.is_poisoned() {
                parts.push(format!("f:{folder}:poisoned"));
            }
        }
        for (path, source) in &self.files {
            if let Some(decision) = source.block.decide(email) {
                parts.push(format!("p:{path}:{}", decision.as_str()));
            } else if source.block.is_poisoned() {
                parts.push(format!("p:{path}:poisoned"));
            }
        }
        parts.sort();
        signature_digest(&parts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn block(allow: &[&str], deny: &[&str]) -> AclBlock {
        AclBlock {
            allow: allow.iter().map(|s| s.to_string()).collect(),
            deny: deny.iter().map(|s| s.to_string()).collect(),
            errors: Vec::new(),
        }
    }

    fn path(raw: &str) -> PathBuf {
        PathBuf::from(raw)
    }

    /// site root grants the team; hr/ narrows to hr; one file denies an intern.
    fn sample_index() -> AclIndex {
        let mut index = AclIndex::new();
        index.insert(&path("index.md"), block(&["team@corp.com"], &[]));
        index.insert(&path("hr/index.md"), block(&["hr@corp.com"], &[]));
        index.insert(&path("hr/comp.md"), block(&[], &["intern@corp.com"]));
        index
    }

    #[test]
    fn identifies_index_files_exactly_as_mdshelf_does() {
        assert!(is_index_file(&path("index.md")));
        assert!(is_index_file(&path("hr/index.md")));
        assert!(
            is_index_file(&path("hr/INDEX.md")),
            "the stem is case-insensitive"
        );

        assert!(!is_index_file(&path("hr/comp.md")));
        assert!(!is_index_file(&path("hr/indexes.md")));

        // These are ordinary pages to mdshelf — `hr/readme.md` is served at `hr/readme`,
        // not as the folder's landing page. Treating them as folder indexes turned a
        // rule meant for one file into a rule over the whole subtree.
        assert!(!is_index_file(&path("hr/README.md")));
        assert!(!is_index_file(&path("hr/readme.md")));
        assert!(!is_index_file(&path("hr/index.markdown")));
    }

    #[test]
    fn computes_parent_folders() {
        assert_eq!(parent_folder(&path("index.md")), "");
        assert_eq!(parent_folder(&path("hr/index.md")), "hr");
        assert_eq!(parent_folder(&path("hr/pay/comp.md")), "hr/pay");
    }

    #[test]
    fn site_rules_reach_a_file_with_no_nearer_rule() {
        let index = sample_index();
        assert!(index.allows(&path("notes/idea.md"), "team@corp.com"));
    }

    #[test]
    fn a_folder_rule_replaces_the_site_rule_for_its_subtree() {
        let index = sample_index();
        // hr/index.md names hr@ but not team@, so team@ falls through to the site rule.
        assert!(index.allows(&path("hr/policy.md"), "hr@corp.com"));
        assert!(
            index.allows(&path("hr/policy.md"), "team@corp.com"),
            "the site rule still applies where the folder rule is silent"
        );
    }

    #[test]
    fn index_rules_govern_the_folder_and_the_index_page_itself() {
        let mut index = AclIndex::new();
        index.insert(&path("hr/index.md"), block(&["hr@corp.com"], &[]));

        // D8: the index page is covered by its own rule.
        assert!(index.allows(&path("hr/index.md"), "hr@corp.com"));
        // ...and so is everything beneath it.
        assert!(index.allows(&path("hr/policy.md"), "hr@corp.com"));
        assert!(index.allows(&path("hr/pay/comp.md"), "hr@corp.com"));
        // Someone unnamed gets nothing.
        assert!(!index.allows(&path("hr/policy.md"), "stranger@corp.com"));
    }

    #[test]
    fn a_file_rule_beats_the_folder_it_lives_in() {
        let index = sample_index();
        let mut index = index;
        index.insert(&path("hr/comp.md"), block(&[], &["hr@corp.com"]));
        assert!(!index.allows(&path("hr/comp.md"), "hr@corp.com"));
        assert!(index.allows(&path("hr/policy.md"), "hr@corp.com"));
    }

    #[test]
    fn a_deep_folder_inherits_from_the_nearest_ancestor_with_rules() {
        let mut index = AclIndex::new();
        index.insert(&path("index.md"), block(&["team@corp.com"], &[]));
        index.insert(&path("a/index.md"), block(&["a@corp.com"], &[]));
        // b/ and c/ declare nothing, so a/'s rules reach all the way down.
        assert!(index.allows(&path("a/b/c/deep.md"), "a@corp.com"));
        assert!(index.allows(&path("a/b/c/deep.md"), "team@corp.com"));
        assert!(!index.allows(&path("a/b/c/deep.md"), "stranger@corp.com"));
    }

    #[test]
    fn an_empty_index_denies_everything() {
        let index = AclIndex::new();
        assert!(index.is_empty());
        assert!(!index.allows(&path("anything.md"), "ana@corp.com"));
    }

    #[test]
    fn blocks_without_rules_do_not_mark_the_site_as_ruled() {
        let mut index = AclIndex::new();
        index.insert(&path("plain.md"), AclBlock::default());
        assert!(index.is_empty(), "a rule-free vault must stay rule-free");
    }

    /// A rule in a file mdshelf serves as an ordinary page governs that page only.
    ///
    /// `readme.md` and `index.markdown` previously widened to the whole folder, so
    /// granting one person one file quietly granted them everything beside it.
    #[test]
    fn a_non_index_file_governs_only_itself() {
        for carrier in ["hr/README.md", "hr/index.markdown"] {
            let mut index = AclIndex::new();
            index.insert(&path(carrier), block(&["contractor@corp.com"], &[]));

            assert!(
                index.allows(&path(carrier), "contractor@corp.com"),
                "{carrier} should still govern itself"
            );
            assert!(
                !index.allows(&path("hr/policy.md"), "contractor@corp.com"),
                "{carrier} must not widen access to its whole folder"
            );
        }
    }

    #[test]
    fn signatures_match_for_viewers_with_identical_access() {
        let index = sample_index();
        // Neither address is named anywhere, so both see exactly the same (empty) site.
        assert_eq!(
            index.signature("nobody-a@corp.com"),
            index.signature("nobody-b@corp.com")
        );
        // Someone the rules do name must differ.
        assert_ne!(
            index.signature("hr@corp.com"),
            index.signature("nobody-a@corp.com")
        );
        assert_ne!(
            index.signature("hr@corp.com"),
            index.signature("team@corp.com")
        );
    }

    #[test]
    fn signatures_are_stable_across_calls() {
        let index = sample_index();
        assert_eq!(
            index.signature("hr@corp.com"),
            index.signature("hr@corp.com")
        );
    }

    #[test]
    fn rows_describe_every_rule_with_its_level() {
        let index = sample_index();
        let rows = index.rows();
        assert!(
            rows.iter()
                .any(|row| row.level == "site" && row.email == "team@corp.com")
        );
        assert!(
            rows.iter()
                .any(|row| row.level == "folder" && row.path == "hr" && row.email == "hr@corp.com")
        );
        assert!(rows.iter().any(|row| row.level == "file"
            && row.path == "hr/comp.md"
            && row.effect == "deny"
            && row.email == "intern@corp.com"));
    }

    #[test]
    fn poisoned_blocks_are_reported_with_their_file() {
        let mut index = AclIndex::new();
        index.insert(
            &path("hr/comp.md"),
            AclBlock {
                allow: Vec::new(),
                deny: Vec::new(),
                errors: vec![AclError {
                    key: "allow".into(),
                    message: "must be a list".into(),
                    line: Some(3),
                }],
            },
        );
        let poisoned = index.poisoned();
        assert_eq!(poisoned.len(), 1);
        assert_eq!(poisoned[0].0, "hr/comp.md");
        assert_eq!(poisoned[0].1.line, Some(3));
    }
}
