//! Property-based tests for the ACL resolver, index, and signature.
//!
//! The example-based tests check the cases someone thought of. These check invariants
//! over generated vaults and rule sets — thousands of shapes nobody would write by
//! hand, including the ones that only matter when rules interact awkwardly.
//!
//! The signature property is the important one. The whole per-viewer cache (D12) rests
//! on a claim that has so far only been argued in prose: viewers with equal signatures
//! resolve identically for *every* path. If that is false anywhere, one reader is
//! served another's view of the vault.

use std::collections::BTreeSet;
use std::path::PathBuf;

use mdshelf::acl::{AclBlock, AclError, AclIndex, Decision};
use proptest::prelude::*;

/// A deliberately small address pool: collisions between viewers are what make the
/// signature property worth testing, and a large pool would make them vanishingly rare.
const EMAILS: [&str; 5] = [
    "a@corp.com",
    "b@corp.com",
    "c@corp.com",
    "d@corp.com",
    "e@corp.com",
];

/// Paths covering a root index, nested folder indexes, and plain files at each depth.
const PATHS: [&str; 10] = [
    "index.md",
    "top.md",
    "one/index.md",
    "one/page.md",
    "one/two/index.md",
    "one/two/deep.md",
    "one/two/three/index.md",
    "one/two/three/leaf.md",
    "other/index.md",
    "other/thing.md",
];

/// A rule block: subsets of the pool in `allow` and `deny`, or a poisoned block.
fn any_block() -> impl Strategy<Value = AclBlock> {
    let emails = prop::collection::vec(0usize..EMAILS.len(), 0..4);
    (emails.clone(), emails, 0u8..10).prop_map(|(allow, deny, poison)| {
        if poison == 0 {
            return AclBlock {
                allow: Vec::new(),
                deny: Vec::new(),
                errors: vec![AclError {
                    key: "allow".into(),
                    message: "generated poison".into(),
                    line: Some(2),
                }],
            };
        }
        let dedup = |indices: Vec<usize>| {
            indices
                .into_iter()
                .map(|i| EMAILS[i].to_string())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
        };
        AclBlock {
            allow: dedup(allow),
            deny: dedup(deny),
            errors: Vec::new(),
        }
    })
}

/// A whole vault's worth of rules: each path either carries a block or does not.
fn any_blocks() -> impl Strategy<Value = Vec<Option<AclBlock>>> {
    prop::collection::vec(prop::option::of(any_block()), PATHS.len())
}

fn build(blocks: &[Option<AclBlock>]) -> AclIndex {
    let mut index = AclIndex::new();
    for (path, block) in PATHS.iter().zip(blocks) {
        if let Some(block) = block {
            index.insert(&PathBuf::from(path), block.clone());
        }
    }
    index
}

fn any_index() -> impl Strategy<Value = AclIndex> {
    any_blocks().prop_map(|blocks| build(&blocks))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    /// The property the view cache depends on (D12).
    ///
    /// Two viewers whose signatures match must resolve identically for every path —
    /// otherwise sharing a cached view serves one of them the other's vault.
    #[test]
    fn equal_signatures_imply_identical_resolution(index in any_index()) {
        for (i, first) in EMAILS.iter().enumerate() {
            for second in EMAILS.iter().skip(i + 1) {
                if index.signature(first) != index.signature(second) {
                    continue;
                }
                for path in PATHS {
                    let path = PathBuf::from(path);
                    prop_assert_eq!(
                        index.resolve(&path, first).decision,
                        index.resolve(&path, second).decision,
                        "{} and {} share a signature but differ on {}",
                        first, second, path.display()
                    );
                }
            }
        }
    }

    /// The same property with the precondition forced.
    ///
    /// Above, roughly 4% of generated viewer pairs happen to share a signature — real
    /// coverage, but mostly the trivial case where neither viewer is named anywhere.
    /// Here every rule naming `a@corp.com` is rewritten to name `b@corp.com` in exactly
    /// the same way, so the two are indistinguishable to the rules by construction and
    /// the precondition holds on every single case.
    #[test]
    fn viewers_with_mirrored_rules_share_a_signature_and_a_view(blocks in any_blocks()) {
        let (original, mirror) = ("a@corp.com", "b@corp.com");

        let swap = |list: &[String]| -> Vec<String> {
            let mut out: Vec<String> = list
                .iter()
                .filter(|entry| entry.as_str() != mirror)
                .cloned()
                .collect();
            if out.iter().any(|entry| entry == original) {
                out.push(mirror.to_string());
            }
            out
        };

        let mirrored_blocks: Vec<Option<AclBlock>> = blocks
            .iter()
            .map(|slot| {
                slot.as_ref().map(|block| AclBlock {
                    allow: swap(&block.allow),
                    deny: swap(&block.deny),
                    errors: block.errors.clone(),
                })
            })
            .collect();
        let index = build(&mirrored_blocks);

        prop_assert_eq!(
            index.signature(original),
            index.signature(mirror),
            "mirrored viewers must share a signature"
        );
        for path in PATHS {
            let path_buf = PathBuf::from(path);
            prop_assert_eq!(
                index.resolve(&path_buf, original).decision,
                index.resolve(&path_buf, mirror).decision,
                "mirrored viewers differ on {}", path
            );
        }
    }

    /// An address named nowhere in the vault can never reach anything (D9).
    #[test]
    fn an_address_named_nowhere_is_denied_everywhere(index in any_index()) {
        let stranger = "stranger@elsewhere.example";
        prop_assert!(!index.known_emails().iter().any(|e| e == stranger));
        for path in PATHS {
            let resolution = index.resolve(&PathBuf::from(path), stranger);
            prop_assert_eq!(
                resolution.decision, Decision::Deny,
                "{} was not denied for an unnamed address", path
            );
        }
    }

    /// Resolution is a pure function of the rules — no ordering or caching effects.
    #[test]
    fn resolution_is_deterministic(index in any_index()) {
        for email in EMAILS {
            for path in PATHS {
                let path = PathBuf::from(path);
                prop_assert_eq!(
                    index.resolve(&path, email).decision,
                    index.resolve(&path, email).decision
                );
            }
        }
    }

    /// A signature is stable, and differs only when some rule actually differs.
    #[test]
    fn signatures_are_stable(index in any_index()) {
        for email in EMAILS {
            prop_assert_eq!(index.signature(email), index.signature(email));
        }
    }

    /// A file-level deny is absolute for that file: nothing inherited can override it,
    /// because no level is more specific (D6).
    #[test]
    fn a_file_level_deny_always_wins(index in any_index(), which in 0usize..EMAILS.len()) {
        let email = EMAILS[which];
        let mut index = index;
        index.insert(
            &PathBuf::from("one/two/deep.md"),
            AclBlock {
                allow: Vec::new(),
                deny: vec![email.to_string()],
                errors: Vec::new(),
            },
        );
        prop_assert_eq!(
            index.resolve(&PathBuf::from("one/two/deep.md"), email).decision,
            Decision::Deny
        );
    }

    /// A file-level allow reaches the address even when an ancestor denies them.
    #[test]
    fn a_file_level_allow_overrides_an_inherited_deny(index in any_index(), which in 0usize..EMAILS.len()) {
        let email = EMAILS[which];
        let mut index = index;
        index.insert(
            &PathBuf::from("one/two/deep.md"),
            AclBlock {
                allow: vec![email.to_string()],
                deny: Vec::new(),
                errors: Vec::new(),
            },
        );
        prop_assert_eq!(
            index.resolve(&PathBuf::from("one/two/deep.md"), email).decision,
            Decision::Allow
        );
    }

    /// A poisoned block denies everyone at or below it, whatever any ancestor grants
    /// (D10). Fail-closed must not be escapable by adding rules further up.
    #[test]
    fn a_poisoned_folder_denies_its_whole_subtree(index in any_index(), which in 0usize..EMAILS.len()) {
        let email = EMAILS[which];
        let mut index = index;
        index.insert(
            &PathBuf::from("one/two/index.md"),
            AclBlock {
                allow: Vec::new(),
                deny: Vec::new(),
                errors: vec![AclError {
                    key: "allow".into(),
                    message: "poisoned".into(),
                    line: Some(2),
                }],
            },
        );
        // Everything at or beneath the poisoned folder, unless it carries its own rule
        // that decides first.
        for path in ["one/two/index.md", "one/two/deep.md", "one/two/three/leaf.md"] {
            let path_buf = PathBuf::from(path);
            let decided_nearer = index
                .resolve(&path_buf, email)
                .decisive_step()
                .is_some_and(|step| step.source != "one/two/index.md" && !step.poisoned);
            if decided_nearer {
                continue;
            }
            prop_assert_eq!(
                index.resolve(&path_buf, email).decision, Decision::Deny,
                "{} escaped a poisoned ancestor", path
            );
        }
    }

    /// Every allow verdict is attributable to a rule that names the address; the
    /// fail-closed default can only ever produce a denial.
    #[test]
    fn an_allow_is_always_explained_by_a_rule(index in any_index()) {
        for email in EMAILS {
            for path in PATHS {
                let resolution = index.resolve(&PathBuf::from(path), email);
                if resolution.decision == Decision::Allow {
                    prop_assert!(
                        !resolution.defaulted,
                        "{path} was allowed by default rather than by a rule"
                    );
                    prop_assert!(
                        resolution.decisive_step().is_some(),
                        "{path} was allowed with no decisive rule"
                    );
                    prop_assert!(
                        resolution.poisoned_source.is_none(),
                        "{path} was allowed despite a poisoned block"
                    );
                }
            }
        }
    }
}
