//! Site search.
//!
//! Three parts, and the split is the security model:
//!
//! - **Corpus.** mdshelf writes every published page's body — frontmatter removed, drafts
//!   left out — into a directory of its own. qmd indexes that directory, never the vault,
//!   so access rules are not searchable text.
//! - **Ranking.** qmd answers a query with an ordered list of corpus files. That list
//!   is a suggestion, not a result: it is computed for nobody in particular.
//! - **Results.** Each suggested file is mapped back to a page and looked up in the
//!   reader's own projection of the site, the same one every page request uses. A page
//!   the reader may not open, a draft, and a file that no longer exists all drop out the
//!   same way, and titles and snippets come from mdshelf's own copy of the page.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::config::SearchConfig;
use crate::content::{Page, Universe, rel_path_key};

/// The most results the search page shows.
pub const PAGE_RESULTS: usize = 20;
/// The most results the search dialog shows while typing.
pub const DIALOG_RESULTS: usize = 8;
/// How many ranked files to ask qmd for. More than either limit, because the reader's
/// access rules remove some of them after ranking.
const CANDIDATES: usize = 60;
/// Longer queries are cut here; nothing useful is lost and qmd is spared the work.
const MAX_QUERY_CHARS: usize = 200;
/// The first query after qmd starts loads the embedding model, which takes seconds on
/// a CPU-only host.
const QMD_TIMEOUT: Duration = Duration::from_secs(30);
/// Characters of page text a snippet holds.
const SNIPPET_CHARS: usize = 220;
/// Characters of context kept before the first match.
const SNIPPET_LEAD: usize = 60;

/// The qmd collection a site's pages go into: its mount without the leading `/`, every
/// remaining `/` replaced by `-` (`/trust-center` → `trust-center`, `/docs/v2` →
/// `docs-v2`).
pub fn collection_name(mount: &str) -> String {
    mount.trim_start_matches('/').replace('/', "-")
}

/// The corpus file for a page, relative to its collection directory.
///
/// qmd indexes `**/*.md`, so a page whose source is not exactly `.md` (`notes.markdown`,
/// `notes.MD`) gets `.md` appended rather than renamed — the mapping back is then
/// unambiguous: try the path as it is, then without the appended suffix.
fn corpus_file(rel_key: &str) -> String {
    if rel_key.ends_with(".md") {
        rel_key.to_string()
    } else {
        format!("{rel_key}.md")
    }
}

/// What a page contributes to the index: its body, under its title when the body does
/// not open with one, so a title-only frontmatter field is still searchable.
fn corpus_text(page: &Page) -> String {
    let opens_with_heading = page
        .body
        .lines()
        .find(|line| !line.trim().is_empty())
        .is_some_and(|line| line.trim_start().starts_with("# "));
    if opens_with_heading {
        page.body.clone()
    } else {
        format!("# {}\n\n{}", page.title, page.body)
    }
}

/// How many corpus files a write changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CorpusChanges {
    pub written: usize,
    pub removed: usize,
}

/// Refuse a corpus that is, contains, or sits inside a site root once symlinks are
/// resolved. mdshelf deletes files in the corpus, and `acl grant` is the only thing
/// allowed to write to a vault, so the two must never be the same place under any name.
pub fn check_corpus_location<'a>(
    corpus: &Path,
    site_roots: impl IntoIterator<Item = &'a Path>,
) -> Result<()> {
    let corpus_real = resolve_existing(corpus)?;
    for root in site_roots {
        let root_real = root
            .canonicalize()
            .with_context(|| format!("resolving {}", root.display()))?;
        if corpus_real.starts_with(&root_real) || root_real.starts_with(&corpus_real) {
            bail!(
                "[search] corpus {} overlaps the site at {} once symlinks are resolved; \
                 mdshelf rewrites and deletes files in the corpus, so it must be a directory \
                 of its own.",
                corpus.display(),
                root.display()
            );
        }
    }
    Ok(())
}

/// `path` with every symlink in its existing part resolved; the part that does not
/// exist yet is appended as written.
fn resolve_existing(path: &Path) -> Result<PathBuf> {
    let mut existing = path;
    let mut missing = Vec::new();
    loop {
        match existing.canonicalize() {
            Ok(real) => {
                return Ok(missing.iter().rev().fold(real, |acc, part| acc.join(part)));
            }
            Err(_) => {
                let (Some(parent), Some(name)) = (existing.parent(), existing.file_name()) else {
                    bail!("cannot resolve {}", path.display());
                };
                missing.push(name.to_os_string());
                existing = parent;
            }
        }
    }
}

/// Bring `corpus` in line with `universe`: one file per published page, nothing else.
///
/// Files whose content is already right are left alone, so qmd's next update sees only
/// real changes. Any other `.md` file under the corpus is deleted, as are directories
/// that end up empty.
///
/// Fails closed on symlinks: the corpus root, every directory below it and every file
/// written must be real, and pruning never follows a link. A link here could point into
/// a vault, and writing or deleting through it would change files nobody asked mdshelf
/// to touch.
pub fn write_corpus(corpus: &Path, universe: &Universe) -> Result<CorpusChanges> {
    check_corpus_location(
        corpus,
        universe.sites().iter().map(|site| site.root.as_path()),
    )?;
    match std::fs::symlink_metadata(corpus) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => bail!(
            "search corpus {} is not a plain directory (a symlink or a file); refusing to write",
            corpus.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(corpus)
                .with_context(|| format!("creating {}", corpus.display()))?;
        }
        Err(error) => {
            return Err(error).with_context(|| format!("reading {}", corpus.display()));
        }
    }

    let mut expected: HashMap<PathBuf, String> = HashMap::new();
    for site in universe.sites() {
        let collection = PathBuf::from(collection_name(&site.mount));
        for page in site.pages().filter(|page| !page.draft) {
            let file = collection.join(corpus_file(&rel_path_key(&page.rel_path)));
            expected.insert(file, corpus_text(page));
        }
    }

    let mut changes = CorpusChanges::default();
    for (relative, text) in &expected {
        if let Some(parent) = relative.parent() {
            ensure_real_dirs(corpus, parent)?;
        }
        let file = corpus.join(relative);
        match std::fs::symlink_metadata(&file) {
            Ok(meta) if meta.is_file() => {
                if std::fs::read_to_string(&file).is_ok_and(|current| &current == text) {
                    continue;
                }
            }
            Ok(_) => bail!(
                "{} in the search corpus is not a plain file (a symlink or a directory); \
                 refusing to write through it",
                file.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", file.display()));
            }
        }
        std::fs::write(&file, text).with_context(|| format!("writing {}", file.display()))?;
        changes.written += 1;
    }

    let keep: HashSet<PathBuf> = expected
        .keys()
        .map(|relative| corpus.join(relative))
        .collect();
    changes.removed = prune(corpus, &keep)?;
    Ok(changes)
}

/// Create each directory of `relative` under `root`, refusing any that exists as a
/// symlink or a file.
fn ensure_real_dirs(root: &Path, relative: &Path) -> Result<()> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => bail!(
                "{} in the search corpus is not a plain directory (a symlink or a file); \
                 refusing to write through it",
                current.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&current)
                    .with_context(|| format!("creating {}", current.display()))?;
            }
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", current.display()));
            }
        }
    }
    Ok(())
}

/// Delete `.md` files under `dir` that are not in `keep`, and directories left empty.
/// Returns the number of files deleted. `dir` itself is kept. Stops at the first
/// symlink rather than follow or delete it.
fn prune(dir: &Path, keep: &HashSet<PathBuf>) -> Result<usize> {
    let mut removed = 0;
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        // `DirEntry::file_type` describes the entry itself and never follows a link.
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            bail!(
                "{} in the search corpus is a symlink; refusing to prune through it",
                path.display()
            );
        }
        if kind.is_dir() {
            removed += prune(&path, keep)?;
            if std::fs::read_dir(&path)?.next().is_none() {
                std::fs::remove_dir(&path)
                    .with_context(|| format!("removing {}", path.display()))?;
            }
        } else if path.extension().is_some_and(|ext| ext == "md") && !keep.contains(&path) {
            std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// One file qmd ranked: the collection and the corpus path within it, decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankedFile {
    pub collection: String,
    pub path: String,
}

impl RankedFile {
    /// Parse qmd's `qmd://<collection>/<percent-encoded path>[?…]`.
    pub fn parse(uri: &str) -> Option<Self> {
        let rest = uri.strip_prefix("qmd://")?;
        let rest = rest.split('?').next().unwrap_or(rest);
        let (collection, path) = rest.split_once('/')?;
        let path = percent_encoding::percent_decode_str(path)
            .decode_utf8()
            .ok()?
            .into_owned();
        if collection.is_empty() || path.is_empty() {
            return None;
        }
        Some(Self {
            collection: collection.to_string(),
            path,
        })
    }

    /// The source paths this corpus file may stand for, in the order to try them.
    pub fn source_candidates(&self) -> Vec<&str> {
        let mut candidates = vec![self.path.as_str()];
        if let Some(stripped) = self.path.strip_suffix(".md") {
            candidates.push(stripped);
        }
        candidates
    }
}

/// The search runtime: the qmd endpoint and an HTTP client for it.
pub struct SearchRuntime {
    query_url: String,
    http: reqwest::Client,
    pub corpus: PathBuf,
}

#[derive(Serialize)]
struct QmdQuery<'a> {
    searches: [QmdSubQuery<'a>; 2],
    rerank: bool,
    limit: usize,
}

#[derive(Serialize)]
struct QmdSubQuery<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    query: &'a str,
}

#[derive(Deserialize)]
struct QmdResponse {
    results: Vec<QmdResult>,
}

#[derive(Deserialize)]
struct QmdResult {
    file: String,
}

impl SearchRuntime {
    pub fn new(config: &SearchConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(QMD_TIMEOUT)
            .build()
            .context("building the search client")?;
        Ok(Self {
            query_url: format!("{}/query", config.qmd_url),
            http,
            corpus: config.corpus.clone(),
        })
    }

    /// Ask qmd to rank the corpus for `query`: keyword and semantic search, fused by
    /// rank, without the language-model reranker.
    pub async fn rank(&self, query: &str) -> Result<Vec<RankedFile>> {
        let body = QmdQuery {
            searches: [
                QmdSubQuery { kind: "lex", query },
                QmdSubQuery { kind: "vec", query },
            ],
            rerank: false,
            limit: CANDIDATES,
        };
        let response = self
            .http
            .post(&self.query_url)
            .json(&body)
            .send()
            .await
            .context("reaching the search index")?;
        if !response.status().is_success() {
            return Err(anyhow!(
                "the search index answered with status {}",
                response.status()
            ));
        }
        let parsed: QmdResponse = response
            .json()
            .await
            .context("reading the search index's answer")?;
        Ok(parsed
            .results
            .iter()
            .filter_map(|result| RankedFile::parse(&result.file))
            .collect())
    }
}

/// The query as searched: trimmed, cut to a sane length on a character boundary.
pub fn normalize_query(raw: &str) -> String {
    raw.trim().chars().take(MAX_QUERY_CHARS).collect()
}

/// One run of snippet text, marked when it matches a query term.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Segment {
    pub text: String,
    #[serde(rename = "match")]
    pub matched: bool,
}

/// The words of a query worth highlighting, lowercased.
fn terms(query: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.chars().count() >= 2)
        .map(str::to_lowercase)
        .filter(|word| seen.insert(word.clone()))
        .take(8)
        .collect()
}

/// A passage of `page` around the first occurrence of a query term, split into
/// highlighted and plain runs. Built from the rendered page, so it holds exactly what
/// the reader would see on the page and nothing from its frontmatter.
pub fn snippet(page: &Page, query: &str) -> Vec<Segment> {
    let text = plain_text(&page.html);
    let mut text = text.as_str();
    // The page heading is shown as the result title already.
    if let Some(rest) = text.strip_prefix(page.title.as_str()) {
        text = rest.trim_start();
    }
    let terms = terms(query);
    let start = anchor(text, &terms)
        .map(|at| back_to_word_start(text, at.saturating_sub(SNIPPET_LEAD)))
        .unwrap_or(0);
    let window: String = text[start..].chars().take(SNIPPET_CHARS).collect();
    let mut segments = highlight(&window, &terms);
    if start > 0 {
        segments.insert(0, plain("… "));
    }
    if text[start..].chars().nth(SNIPPET_CHARS).is_some() {
        segments.push(plain(" …"));
    }
    segments
}

fn plain(text: &str) -> Segment {
    Segment {
        text: text.to_string(),
        matched: false,
    }
}

/// Where the snippet centres: the first occurrence of the longest query term the page
/// contains. The longest term is usually the most specific one, so "who stores our
/// customer data" shows the passage about customers rather than the first "who".
fn anchor(text: &str, terms: &[String]) -> Option<usize> {
    let mut by_length: Vec<&String> = terms.iter().collect();
    by_length.sort_by_key(|term| std::cmp::Reverse(term.chars().count()));
    by_length.into_iter().find_map(|term| {
        text.char_indices()
            .map(|(at, _)| at)
            .find(|&at| matches_at(text, at, term))
    })
}

/// Whether a word starting at `at` begins with `term`, case-insensitively. Matching
/// only at word starts keeps "our" from lighting up inside "tour".
fn matches_at(text: &str, at: usize, term: &str) -> bool {
    let starts_word = text[..at]
        .chars()
        .next_back()
        .is_none_or(|previous| !previous.is_alphanumeric());
    if !starts_word {
        return false;
    }
    let mut candidate = text[at..].chars().flat_map(char::to_lowercase);
    term.chars()
        .all(|expected| candidate.next() == Some(expected))
}

/// Length in bytes of the text starting at `at` that spells `term` case-insensitively.
fn match_len(text: &str, at: usize, term: &str) -> usize {
    let wanted = term.chars().count();
    let mut lowered = 0;
    let mut bytes = 0;
    for character in text[at..].chars() {
        if lowered >= wanted {
            break;
        }
        lowered += character.to_lowercase().count();
        bytes += character.len_utf8();
    }
    bytes
}

/// Move `at` back to the start of the word it falls in, without passing `0`.
fn back_to_word_start(text: &str, mut at: usize) -> usize {
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    let before = &text[..at];
    match before.rfind(char::is_whitespace) {
        Some(space) if at - space < 24 => {
            space + before[space..].chars().next().map_or(1, char::len_utf8)
        }
        _ => at,
    }
}

fn highlight(window: &str, terms: &[String]) -> Vec<Segment> {
    let mut segments: Vec<Segment> = Vec::new();
    let mut push = |text: &str, matched: bool| {
        if text.is_empty() {
            return;
        }
        match segments.last_mut() {
            Some(last) if last.matched == matched => last.text.push_str(text),
            _ => segments.push(Segment {
                text: text.to_string(),
                matched,
            }),
        }
    };
    let mut at = 0;
    let mut plain_from = 0;
    while at < window.len() {
        let hit = terms
            .iter()
            .filter(|term| matches_at(window, at, term))
            .map(|term| match_len(window, at, term))
            .max();
        match hit {
            Some(len) if len > 0 => {
                push(&window[plain_from..at], false);
                push(&window[at..at + len], true);
                at += len;
                plain_from = at;
            }
            _ => at += window[at..].chars().next().map_or(1, char::len_utf8),
        }
    }
    push(&window[plain_from..], false);
    segments
}

/// The visible text of rendered page HTML: tags dropped, `script` and `style` contents
/// dropped, the common entities decoded, whitespace collapsed.
pub fn plain_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut rest = html;
    while let Some(open) = rest.find('<') {
        push_text(&mut out, &rest[..open]);
        let after = &rest[open..];
        let skip_until = ["script", "style"].iter().find_map(|tag| {
            let opening = format!("<{tag}");
            after
                .get(..opening.len())
                .filter(|head| head.eq_ignore_ascii_case(&opening))
                .map(|_| format!("</{tag}>"))
        });
        rest = match skip_until {
            Some(closing) => match find_ignore_ascii_case(after, &closing) {
                Some(end) => &after[end + closing.len()..],
                None => "",
            },
            None => match after.find('>') {
                Some(end) => &after[end + 1..],
                None => "",
            },
        };
        out.push(' ');
    }
    push_text(&mut out, rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn find_ignore_ascii_case(haystack: &str, needle: &str) -> Option<usize> {
    haystack.char_indices().map(|(at, _)| at).find(|&at| {
        haystack
            .get(at..at + needle.len())
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(needle))
    })
}

fn push_text(out: &mut String, text: &str) {
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp..];
        match after.find(';').filter(|&end| end <= 10) {
            Some(end) => match decode_entity(&after[1..end]) {
                Some(decoded) => {
                    out.push(decoded);
                    rest = &after[end + 1..];
                }
                None => {
                    out.push('&');
                    rest = &after[1..];
                }
            },
            None => {
                out.push('&');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
}

fn decode_entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some(' '),
        _ => {
            let number = name.strip_prefix('#')?;
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse().ok()?,
            };
            char::from_u32(code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranked_files_decode_back_to_their_source_paths() {
        let ranked = RankedFile::parse("qmd://docs/Odd%20Dir/My%20File.md?index=founders").unwrap();
        assert_eq!(ranked.collection, "docs");
        assert_eq!(
            ranked.source_candidates(),
            ["Odd Dir/My File.md", "Odd Dir/My File"]
        );

        let markdown = RankedFile::parse("qmd://docs/notes.markdown.md").unwrap();
        assert_eq!(
            markdown.source_candidates(),
            ["notes.markdown.md", "notes.markdown"]
        );

        for malformed in ["docs/a.md", "qmd://docs", "qmd:///a.md", "qmd://docs/"] {
            assert_eq!(RankedFile::parse(malformed), None, "{malformed}");
        }
    }

    #[test]
    fn collection_names_follow_the_mount() {
        assert_eq!(collection_name("/workspace"), "workspace");
        assert_eq!(collection_name("/docs/v2"), "docs-v2");
    }

    #[test]
    fn plain_text_keeps_what_a_reader_sees() {
        let html = "<h1>Title</h1><p>Fish &amp; chips &lt;3 &#x2014; caf&eacute;</p>\
                    <script>alert('x')</script><STYLE>p{}</STYLE><pre><code>let a = 1;</code></pre>";
        assert_eq!(
            plain_text(html),
            "Title Fish & chips <3 \u{2014} caf&eacute; let a = 1;"
        );
    }

    fn text(segments: &[Segment]) -> String {
        segments
            .iter()
            .map(|s| {
                if s.matched {
                    format!("[{}]", s.text)
                } else {
                    s.text.clone()
                }
            })
            .collect()
    }

    #[test]
    fn highlighting_is_case_insensitive_at_word_starts_and_prefers_the_longest_term() {
        let segments = highlight(
            "Deploy the deployment; ŻÓŁW and żółwie. Our tour of metadata.",
            &terms("deploy deployment żółw our data"),
        );
        assert_eq!(
            text(&segments),
            "[Deploy] the [deployment]; [ŻÓŁW] and [żółw]ie. [Our] tour of metadata."
        );
    }
}
