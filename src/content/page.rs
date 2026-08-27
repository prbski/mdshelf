use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use gray_matter::{Matter, ParsedEntity, engine::YAML};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::acl::{AclBlock, parse_acl, strip_acl_keys};
use crate::render::markdown::{Heading, LinkCtx, MarkdownRenderer};

/// One rendered Markdown page within a site.
#[derive(Debug, Clone, Serialize)]
pub struct Page {
    pub fs_path: PathBuf,
    pub rel_path: PathBuf,
    /// URL path within the site (no leading slash, no extension). Empty for the root index.
    pub url_path: String,
    /// Absolute URL with the site's mount prefix (e.g. "/notes/guide/intro").
    pub url: String,
    pub is_index: bool,
    pub title: String,
    /// Basename of the source file (e.g. `welcome.md`).
    pub filename: String,
    /// File modification time in milliseconds since the Unix epoch.
    pub modified_at_ms: i64,
    pub description: Option<String>,
    pub layout: String,
    pub sidebar_order: Option<i64>,
    pub draft: bool,
    /// Frontmatter with the access-rule keys removed (SEC-6). This is what templates
    /// see, so invitee addresses cannot reach rendered HTML.
    pub frontmatter: JsonValue,
    pub headings: Vec<Heading>,
    pub html: String,
    /// The Markdown source this page was rendered from: frontmatter removed, CRLF
    /// normalized to LF, and exactly one trailing newline (empty stays empty).
    ///
    /// Captured *after* the frontmatter split, so `allow`/`deny` were never part of it —
    /// the same structural guarantee `frontmatter` gets from `strip_acl_keys`, except
    /// here there is nothing to strip.
    ///
    /// Skipped during serialization for the same reason `assets` and `acl` are: nothing
    /// that renders a `Page` generically has any business emitting its source. The one
    /// surface that does — the page-actions block — receives it explicitly through
    /// `PageContext::source_escaped`.
    #[serde(skip)]
    pub body: String,
    /// Site-relative paths of the in-vault files this page's rendered HTML points at
    /// (S7/§6.4).
    ///
    /// Computed once, here, where the page is already rendered, and shared by every
    /// link to the page rather than stored per link. It is the *complete* answer to
    /// "what may a link reader fetch besides the page itself" — SEC-4 is enforced by
    /// membership of this set and nothing else.
    ///
    /// Skipped during serialization for the same reason `acl` is: it describes the
    /// vault's shape, and templates have no business rendering it.
    #[serde(skip)]
    pub assets: BTreeSet<String>,
    /// Access rules declared by this file. Skipped during serialization so it can never
    /// be rendered into a page by accident.
    #[serde(skip)]
    pub acl: AclBlock,
}

#[derive(Debug, Default, Deserialize)]
struct FrontmatterFields {
    title: Option<String>,
    description: Option<String>,
    layout: Option<String>,
    sidebar_order: Option<i64>,
    #[serde(default)]
    draft: bool,
}

impl Page {
    pub fn load(
        site_root: &Path,
        rel_path: &Path,
        mount: &str,
        renderer: &MarkdownRenderer,
    ) -> Result<Page> {
        let fs_path = site_root.join(rel_path);
        let raw = std::fs::read_to_string(&fs_path)
            .with_context(|| format!("reading {}", fs_path.display()))?;

        // Strip a UTF-8 byte-order mark before anything looks at the text.
        //
        // Notepad and older PowerShell write one by default, and it is invisible in
        // every editor. Left in place it hides the opening `---` from the frontmatter
        // parser, so the file appears to have no frontmatter at all — which means its
        // `allow`/`deny` rules are silently ignored and the file inherits whatever a
        // broader rule grants. That is a fail-open, and the one outcome this feature
        // must never produce.
        let raw = raw.strip_prefix('\u{feff}').unwrap_or(&raw).to_string();

        let matter = Matter::<YAML>::new();
        let parsed: ParsedEntity<JsonValue> = matter
            .parse(&raw)
            .map_err(|err| anyhow::anyhow!("front matter in {}: {}", fs_path.display(), err))?;
        let body = parsed.content;
        let frontmatter_value = parsed.data.unwrap_or(JsonValue::Object(Default::default()));
        let typed: FrontmatterFields =
            serde_json::from_value(frontmatter_value.clone()).unwrap_or_default();

        // Parse the rules, then strip them: everything downstream of this point sees
        // frontmatter with no addresses in it (SEC-6).
        let acl = parse_acl(&frontmatter_value, &raw);
        let frontmatter_value = strip_acl_keys(frontmatter_value);

        let (url_path, is_index) = url_path_from_rel(rel_path);
        let url = join_url(mount, &url_path);

        let link_ctx = LinkCtx {
            mount: mount.to_string(),
            current_dir: rel_path.parent().map(Path::to_path_buf).unwrap_or_default(),
        };
        let rendered = renderer.render(&body, &link_ctx);

        // Normalize a copy rather than the text handed to comrak: rendering the
        // normalized form would change the HTML of every CRLF file, and "without this
        // feature, behaviour is byte-identical to today" is cheaper to keep than to
        // prove.
        let source = normalize_source(&body);

        let title = typed
            .title
            .clone()
            .or_else(|| {
                rendered
                    .headings
                    .iter()
                    .find(|h| h.level == 1)
                    .map(|h| h.text.clone())
            })
            .unwrap_or_else(|| derive_title_from_path(rel_path));
        let layout = typed.layout.unwrap_or_else(|| "doc".to_string());
        let filename = rel_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled.md".to_string());
        let modified_at_ms = file_modified_at_ms(&fs_path);
        let assets = referenced_assets(site_root, mount, &rendered.html);

        Ok(Page {
            fs_path,
            rel_path: rel_path.to_path_buf(),
            url_path,
            url,
            is_index,
            title,
            filename,
            modified_at_ms,
            description: typed.description,
            layout,
            sidebar_order: typed.sidebar_order,
            draft: typed.draft,
            frontmatter: frontmatter_value,
            html: rendered.html,
            headings: rendered.headings,
            body: source,
            assets,
            acl,
        })
    }

    /// The exact bytes "Copy as Markdown" and "Download Markdown" produce.
    ///
    /// A document that opens with its own `# Heading` is emitted untouched; anything
    /// else gains one, so a pasted page always carries a title. The question "does it
    /// open with an H1" is answered from `headings`, which comrak already built — asking
    /// the text directly would have to know that a `#` inside a fenced code block is not
    /// a heading.
    pub fn source_text(&self) -> String {
        let opens_with_h1 = self.headings.first().is_some_and(|h| h.level == 1);
        compose_source_text(&self.body, &self.title, opens_with_h1)
    }
}

/// CRLF to LF, exactly one trailing newline, and an empty body stays empty.
///
/// Nothing else is touched: leading whitespace can be load-bearing inside an indented
/// code block, and a round-trip back into a vault should not silently reflow the file.
fn normalize_source(body: &str) -> String {
    let lf = if body.contains('\r') {
        body.replace("\r\n", "\n")
    } else {
        body.to_string()
    };
    let trimmed = lf.trim_end_matches('\n');
    if trimmed.is_empty() {
        String::new()
    } else if trimmed.len() == lf.len() {
        let mut lf = lf;
        lf.push('\n');
        lf
    } else {
        let mut out = String::with_capacity(trimmed.len() + 1);
        out.push_str(trimmed);
        out.push('\n');
        out
    }
}

/// Shared by `Page::source_text` and its tests, which is why it takes parts rather than
/// a whole `Page`: the rule is worth testing without building one.
fn compose_source_text(body: &str, title: &str, opens_with_h1: bool) -> String {
    if body.is_empty() {
        return format!("# {title}\n");
    }
    if opens_with_h1 {
        return body.to_string();
    }
    format!("# {title}\n\n{body}")
}

/// Neutralize the only three sequences that can escape a `<script>` element.
///
/// `</` would close it outright. The four-character HTML comment opener is subtler: it
/// switches the tokenizer into *script data escaped* state, after which the genuine
/// `</script>` is no longer recognized as a closer and the rest of the document is
/// swallowed. A backslash is escaped first so the transformation stays invertible.
///
/// HTML entity escaping is **not** an option here: entities are not decoded inside a
/// script element, so `&lt;/script&gt;` would come back out of `textContent` as those
/// literal characters and corrupt the round-trip.
pub fn escape_for_script_block(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    let mut rest = text;
    while let Some(idx) = rest.find(['\\', '<']) {
        out.push_str(&rest[..idx]);
        let tail = &rest[idx..];
        if let Some(after) = tail.strip_prefix('\\') {
            out.push_str("\\\\");
            rest = after;
        } else if let Some(after) = tail.strip_prefix("</") {
            out.push_str("<\\/");
            rest = after;
        } else if let Some(after) = tail.strip_prefix("<!--") {
            out.push_str("<\\!--");
            rest = after;
        } else {
            out.push('<');
            rest = &tail['<'.len_utf8()..];
        }
    }
    out.push_str(rest);
    out
}

/// The characters RFC 5987 lets appear unencoded in an `ext-value`.
const ATTR_CHAR_EXTRA: &[u8] = b"!#$&+-.^_`|~";

/// A `Content-Disposition` value that names `filename` on every client.
///
/// Both parameters are emitted deliberately. `filename*` carries the real name and is
/// what any current browser reads; the plain `filename` is an ASCII reduction, so a
/// client that ignores `filename*` saves something safe instead of inventing a name
/// from the URL path or writing mojibake.
pub fn content_disposition_attachment(filename: &str) -> String {
    format!(
        "attachment; filename=\"{}\"; filename*=UTF-8''{}",
        ascii_fallback_filename(filename),
        rfc5987_encode(filename)
    )
}

/// Percent-encode every byte RFC 5987 does not allow bare.
fn rfc5987_encode(name: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(name.len());
    for &byte in name.as_bytes() {
        if byte.is_ascii_alphanumeric() || ATTR_CHAR_EXTRA.contains(&byte) {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push(HEX[usize::from(byte >> 4)] as char);
            out.push(HEX[usize::from(byte & 0x0f)] as char);
        }
    }
    out
}

/// The quoted-string fallback: printable ASCII only, with the characters that would
/// break out of the quotes or imply a path replaced.
///
/// A name that reduces to nothing but separators carries no information, so it is
/// replaced outright rather than emitted as a row of underscores.
fn ascii_fallback_filename(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for &byte in name.as_bytes() {
        let printable = (0x20..0x7f).contains(&byte);
        let structural = matches!(byte, b'"' | b'\\' | b'/');
        out.push(if printable && !structural {
            byte as char
        } else {
            '_'
        });
    }
    if out.bytes().all(|byte| byte == b'_' || byte == b'.') {
        return "page.md".to_string();
    }
    out
}

/// The attributes whose values are URLs mdshelf has to know about.
///
/// `src` and `href` are what §6.4 names; `poster` is included because a video poster is
/// an image that would otherwise render as a broken box in the reading view.
const URL_ATTRIBUTES: [&str; 3] = ["src", "href", "poster"];

/// Visit every URL-valued attribute in rendered HTML, in order.
///
/// A deliberately small scanner rather than a parser. It is safe because comrak escapes
/// `"` and `'` inside text, so an `href="` sequence in prose cannot reach the output as
/// literal characters — every match is a real attribute.
pub fn visit_attribute_urls(html: &str, mut visit: impl FnMut(&str)) {
    rewrite_attribute_urls_inner(html, &mut |url| {
        visit(url);
        None
    });
}

/// Rewrite URL-valued attributes, replacing a value wherever `map` returns `Some`.
pub fn rewrite_attribute_urls(html: &str, mut map: impl FnMut(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(html.len());
    let mut cursor = 0usize;
    rewrite_attribute_urls_inner(html, &mut |url| map(url))
        .into_iter()
        .for_each(|(start, end, replacement)| {
            out.push_str(&html[cursor..start]);
            out.push_str(&replacement);
            cursor = end;
        });
    out.push_str(&html[cursor..]);
    out
}

/// The shared scan. Returns the byte ranges of the attribute values `map` replaced.
fn rewrite_attribute_urls_inner(
    html: &str,
    map: &mut dyn FnMut(&str) -> Option<String>,
) -> Vec<(usize, usize, String)> {
    let bytes = html.as_bytes();
    let mut replacements = Vec::new();
    let mut index = 0usize;

    while index < bytes.len() {
        let Some(equals) = html[index..].find('=').map(|offset| index + offset) else {
            break;
        };
        index = equals + 1;

        let Some(attribute) = attribute_before(html, equals) else {
            continue;
        };
        if !URL_ATTRIBUTES.contains(&attribute) {
            continue;
        }
        let quote = match bytes.get(equals + 1) {
            Some(b'"') => b'"',
            Some(b'\'') => b'\'',
            _ => continue,
        };
        let value_start = equals + 2;
        let Some(value_end) = html[value_start..]
            .find(quote as char)
            .map(|offset| value_start + offset)
        else {
            break;
        };
        if let Some(replacement) = map(&html[value_start..value_end]) {
            replacements.push((value_start, value_end, replacement));
        }
        index = value_end + 1;
    }
    replacements
}

/// The attribute name immediately before `equals`, if it is a bare ASCII name preceded
/// by whitespace or `<`.
///
/// The whitespace requirement is what keeps `data-src=` from being read as `src=`.
fn attribute_before(html: &str, equals: usize) -> Option<&str> {
    let bytes = html.as_bytes();
    let mut start = equals;
    while start > 0 {
        let byte = bytes[start - 1];
        if byte.is_ascii_alphanumeric() {
            start -= 1;
        } else {
            break;
        }
    }
    if start == equals {
        return None;
    }
    let boundary = bytes.get(start.wrapping_sub(1)).copied();
    match boundary {
        Some(b) if b.is_ascii_whitespace() || b == b'<' => Some(&html[start..equals]),
        _ => None,
    }
}

/// The set of in-vault files a rendered page points at (§6.4).
///
/// Only absolute URLs under the site's own mount count, because that is exactly what
/// mdshelf's own link rewriting emits. Markdown sources are excluded: SEC-4 says a link
/// never reaches the raw source of the page it serves, let alone anybody else's.
fn referenced_assets(site_root: &Path, mount: &str, html: &str) -> BTreeSet<String> {
    let mount_prefix = if mount == "/" {
        "/".to_string()
    } else {
        format!("{mount}/")
    };
    let mut assets = BTreeSet::new();
    visit_attribute_urls(html, |url| {
        let Some(relative) = in_vault_relative(url, &mount_prefix) else {
            return;
        };
        let candidate = Path::new(&relative);
        if crate::content::source::is_markdown_path(candidate) {
            return;
        }
        if crate::content::source::relative_path_has_hidden_component(candidate) {
            return;
        }
        // Resolve through the filesystem exactly as the server does, so a link can
        // never name a file the server would refuse to open.
        let Some(true_rel) = crate::content::source::true_relative_path(site_root, candidate)
        else {
            return;
        };
        if !site_root.join(&true_rel).is_file() {
            return;
        }
        assets.insert(true_rel.to_string_lossy().replace('\\', "/"));
    });
    assets
}

/// The site-relative path an in-vault URL names, or `None` if it points elsewhere.
///
/// Query strings and fragments are dropped: they are addressing detail, not part of the
/// file's identity.
pub fn in_vault_relative(url: &str, mount_prefix: &str) -> Option<String> {
    if url.is_empty() || url.starts_with('#') || url.contains("://") || url.starts_with("//") {
        return None;
    }
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let tail = path.strip_prefix(mount_prefix)?;
    if tail.is_empty() {
        return None;
    }
    let decoded = percent_encoding::percent_decode_str(tail)
        .decode_utf8_lossy()
        .into_owned();
    if decoded
        .split('/')
        .any(|segment| segment == ".." || segment.is_empty())
    {
        return None;
    }
    Some(decoded)
}

/// Translate a relative file path inside a site root into a clean URL path
/// (without leading slash and without `.md` extension).
pub fn url_path_from_rel(rel: &Path) -> (String, bool) {
    let mut parts: Vec<String> = rel
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    let last = parts.pop().unwrap_or_default();
    let stem = last.strip_suffix(".md").unwrap_or(&last);
    let is_index = stem.eq_ignore_ascii_case("index");
    if !is_index {
        parts.push(stem.to_string());
    }
    (parts.join("/"), is_index)
}

/// The page-map keys a URL path may refer to, in priority order.
///
/// One definition, shared by the server and the CLI. Two of them drifted: the server
/// stripped a `.md` suffix case-insensitively and the CLI did not, so
/// `acl explain /docs/hr/comp.MD` resolved to a different page than the server serves
/// — and reported *allow* where the server denies. Any second implementation of "which
/// page is this URL?" will drift the same way.
pub fn page_lookup_keys(url_path: &str) -> Vec<String> {
    let trimmed = url_path.trim_matches('/');
    if trimmed.is_empty() {
        return vec![String::new()];
    }

    // An explicit markdown extension names one page and only that page.
    if let Some((base, extension)) = trimmed.rsplit_once('.')
        && (extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown"))
    {
        return vec![base.to_string()];
    }

    // Otherwise the URL may name the page directly, or the folder it indexes.
    vec![trimmed.to_string(), format!("{trimmed}/index")]
}

/// Compose the absolute URL of a page from its mount and url_path.
pub fn join_url(mount: &str, url_path: &str) -> String {
    let m = if mount == "/" { "" } else { mount };
    if url_path.is_empty() {
        if m.is_empty() {
            "/".to_string()
        } else {
            m.to_string()
        }
    } else {
        format!("{}/{}", m, url_path)
    }
}

fn file_modified_at_ms(path: &Path) -> i64 {
    std::fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn derive_title_from_path(rel: &Path) -> String {
    let stem = rel.file_stem().map(|s| s.to_string_lossy().into_owned());
    match stem {
        Some(name) if name.eq_ignore_ascii_case("index") => rel
            .parent()
            .and_then(|p| p.file_name())
            .map(|s| humanize(&s.to_string_lossy()))
            .unwrap_or_else(|| "Home".to_string()),
        Some(name) => humanize(&name),
        None => "Untitled".to_string(),
    }
}

/// "getting-started" -> "Getting Started", "01-intro" -> "Intro".
pub fn humanize(slug: &str) -> String {
    let cleaned = slug.trim_start_matches(|c: char| c.is_ascii_digit() || c == '-' || c == '_');
    let cleaned = if cleaned.is_empty() { slug } else { cleaned };
    let mut out = String::with_capacity(cleaned.len());
    for (idx, word) in cleaned
        .split(['-', '_', ' '])
        .filter(|w| !w.is_empty())
        .enumerate()
    {
        if idx > 0 {
            out.push(' ');
        }
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.extend(chars);
        }
    }
    if out.is_empty() {
        slug.to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scanner_finds_only_real_url_attributes() {
        let html = concat!(
            "<p>write href=&quot;/docs/not-an-attribute&quot; in prose</p>",
            "<img src=\"/docs/img/a.png\" data-src=\"/docs/img/decoy.png\">",
            "<a href='/docs/other'>x</a>",
            "<video poster=\"/docs/img/b.jpg\"></video>",
            "<div class=\"src\">nope</div>"
        );
        let mut found = Vec::new();
        visit_attribute_urls(html, |url| found.push(url.to_string()));
        assert_eq!(
            found,
            vec!["/docs/img/a.png", "/docs/other", "/docs/img/b.jpg"],
            "data-src, prose, and a class named `src` are not URL attributes"
        );
    }

    #[test]
    fn rewriting_replaces_only_what_the_mapper_claims() {
        let html = "<img src=\"/docs/a.png\"><a href=\"https://example.com\">x</a>";
        let rewritten = rewrite_attribute_urls(html, |url| {
            url.strip_prefix("/docs/")
                .map(|tail| format!("/s/tok/{tail}"))
        });
        assert_eq!(
            rewritten,
            "<img src=\"/s/tok/a.png\"><a href=\"https://example.com\">x</a>"
        );
    }

    /// The JavaScript decoder from the spec, ported: on a backslash, take the next
    /// character literally. Kept in the tests because production has no reason to
    /// decode — the browser does that — but the encoder is worthless if it is not
    /// invertible, and this is what proves it.
    fn unescape_from_script_block(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut chars = text.chars();
        while let Some(ch) = chars.next() {
            if ch == '\\' {
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            } else {
                out.push(ch);
            }
        }
        out
    }

    #[test]
    fn escaping_a_script_block_is_invertible() {
        for source in [
            "",
            "plain prose",
            "an example: </script> ends a block",
            "</SCRIPT> and </script and </ alone",
            "<!-- an html comment --> in markdown",
            "<!-- unterminated",
            "a lone \\ backslash",
            "a double \\\\ backslash",
            "the literal three characters <\\/ verbatim",
            "\\<\\/script\\>",
            "<div>markup</div>\n```html\n<!--x--></script>\n```\n",
            "unicode ok: настройка — <!--",
            "<",
            "\\",
        ] {
            let escaped = escape_for_script_block(source);
            assert_eq!(
                unescape_from_script_block(&escaped),
                source,
                "round-trip failed for {source:?} (escaped to {escaped:?})"
            );
        }
    }

    #[test]
    fn escaping_neutralizes_every_sequence_that_escapes_a_script_element() {
        let escaped = escape_for_script_block("</script><!--");
        assert_eq!(escaped, "<\\/script><\\!--");
        assert!(
            !escaped.contains("</"),
            "a closing-tag opener survived: {escaped:?}"
        );
        assert!(
            !escaped.contains("<!--"),
            "a comment opener survived: {escaped:?}"
        );
    }

    #[test]
    fn a_body_that_opens_with_an_h1_is_emitted_untouched() {
        let body = "# Setup\n\nInstall it.\n";
        assert_eq!(compose_source_text(body, "Frontmatter Title", true), body);
    }

    #[test]
    fn a_body_whose_first_heading_is_not_an_h1_gains_a_title() {
        let body = "## Details\n\nText.\n";
        assert_eq!(
            compose_source_text(body, "Setup", false),
            "# Setup\n\n## Details\n\nText.\n"
        );
    }

    #[test]
    fn a_body_with_no_heading_at_all_gains_a_title() {
        assert_eq!(
            compose_source_text("Just a paragraph.\n", "Setup", false),
            "# Setup\n\nJust a paragraph.\n"
        );
    }

    #[test]
    fn an_empty_body_becomes_a_lone_title() {
        assert_eq!(compose_source_text("", "Setup", false), "# Setup\n");
        assert_eq!(compose_source_text("", "Setup", true), "# Setup\n");
    }

    #[test]
    fn a_hash_inside_a_fenced_code_block_is_not_a_heading() {
        // The reason `source_text` asks `headings` instead of the text: a shell comment
        // reads exactly like an ATX heading, and only the parser knows the difference.
        let renderer = MarkdownRenderer::new();
        let ctx = LinkCtx {
            mount: "/docs".to_string(),
            current_dir: PathBuf::new(),
        };
        let body = "```bash\n# not a heading\n```\n\n# Real Heading\n";
        let rendered = renderer.render(body, &ctx);
        assert!(
            rendered.headings.first().is_some_and(|h| h.level == 1),
            "expected the first heading to be the real H1, got {:?}",
            rendered.headings
        );
        let opens_with_h1 = rendered.headings.first().is_some_and(|h| h.level == 1);
        assert_eq!(
            compose_source_text(body, "Ignored", opens_with_h1),
            body,
            "a document whose first heading is an H1 must not gain a second one"
        );
    }

    #[test]
    fn normalizing_a_source_fixes_line_endings_and_the_trailing_newline() {
        assert_eq!(normalize_source("a\r\nb\r\n"), "a\nb\n");
        assert_eq!(normalize_source("a\n\n\n"), "a\n");
        assert_eq!(normalize_source("a"), "a\n");
        assert_eq!(normalize_source("a\n"), "a\n");
        assert_eq!(normalize_source(""), "");
        assert_eq!(normalize_source("\n\n"), "");
        assert_eq!(
            normalize_source("    indented\n"),
            "    indented\n",
            "leading whitespace is load-bearing inside an indented code block"
        );
    }

    #[test]
    fn a_content_disposition_names_the_file_on_every_client() {
        assert_eq!(
            content_disposition_attachment("setup.md"),
            "attachment; filename=\"setup.md\"; filename*=UTF-8''setup.md"
        );
        assert_eq!(
            content_disposition_attachment("café notes.md"),
            "attachment; filename=\"caf__ notes.md\"; filename*=UTF-8''caf%C3%A9%20notes.md"
        );
        // A Cyrillic stem still ends in an ASCII `.md`, so the reduction keeps real
        // information and is emitted. `filename*` is what any current browser reads
        // anyway; the quoted form only has to be safe.
        assert_eq!(
            content_disposition_attachment("настройка.md"),
            "attachment; filename=\"__________________.md\"; \
             filename*=UTF-8''%D0%BD%D0%B0%D1%81%D1%82%D1%80%D0%BE%D0%B9%D0%BA%D0%B0.md"
        );
        // With nothing ASCII left at all the reduction says nothing, so it is replaced.
        assert_eq!(
            content_disposition_attachment("настройка"),
            "attachment; filename=\"page.md\"; \
             filename*=UTF-8''%D0%BD%D0%B0%D1%81%D1%82%D1%80%D0%BE%D0%B9%D0%BA%D0%B0"
        );
        assert_eq!(
            content_disposition_attachment(""),
            "attachment; filename=\"page.md\"; filename*=UTF-8''"
        );
    }

    #[test]
    fn a_content_disposition_cannot_break_out_of_its_own_quotes() {
        let value = content_disposition_attachment("a\"b\\c/d\ne.md");
        let quoted = value
            .split_once("filename=\"")
            .and_then(|(_, tail)| tail.split_once('"'))
            .map(|(name, _)| name)
            .expect("a quoted filename parameter");
        assert_eq!(quoted, "a_b_c_d_e.md");
    }

    #[test]
    fn rewriting_an_empty_or_attribute_free_document_is_a_no_op() {
        for html in [
            "",
            "<p>plain</p>",
            "<img>",
            "<img src=>",
            "<img src=\"unclosed",
        ] {
            let rewritten = rewrite_attribute_urls(html, |_| Some("X".to_string()));
            assert!(
                rewritten.len() >= html.len().saturating_sub(8),
                "the scanner must not drop content for {html:?}"
            );
        }
    }

    #[test]
    fn in_vault_relative_accepts_only_paths_under_the_mount() {
        assert_eq!(
            in_vault_relative("/docs/img/a.png", "/docs/").as_deref(),
            Some("img/a.png")
        );
        assert_eq!(
            in_vault_relative("/docs/img/a%20b.png?v=2#top", "/docs/").as_deref(),
            Some("img/a b.png")
        );
        for outside in [
            "https://example.com/docs/a.png",
            "//example.com/docs/a.png",
            "/other/a.png",
            "#anchor",
            "",
            "/docs/",
            "/docs/../secret.png",
            "/docs//a.png",
        ] {
            assert!(
                in_vault_relative(outside, "/docs/").is_none(),
                "{outside} is not an in-vault asset"
            );
        }
    }

    #[test]
    fn a_loaded_body_carries_no_frontmatter_and_no_access_rules() {
        // The invariant that makes this feature safe: `body` is captured after the
        // frontmatter split, so there is no `allow`/`deny` in it to strip. A regression
        // here publishes invitee addresses to every reader of the page.
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("comp.md"),
            "---\ntitle: Compensation 2026\nallow:\n  - ana@corp.com\ndeny:\n  \
             - intern@corp.com\n---\n# Compensation 2026\r\n\r\nNumbers.\r\n\r\n\r\n",
        )
        .expect("write");

        let renderer = MarkdownRenderer::new();
        let page =
            Page::load(dir.path(), Path::new("comp.md"), "/hr", &renderer).expect("the page loads");

        assert!(
            page.body.contains("Numbers."),
            "the body kept its prose: {:?}",
            page.body
        );
        for forbidden in ["allow", "deny", "ana@corp.com", "intern@corp.com", "---"] {
            assert!(
                !page.body.contains(forbidden),
                "{forbidden:?} reached the body: {:?}",
                page.body
            );
        }
        assert!(
            !page.body.contains('\r'),
            "CRLF survived normalization: {:?}",
            page.body
        );
        assert!(
            page.body.ends_with("Numbers.\n"),
            "expected exactly one trailing newline, got {:?}",
            page.body
        );
        assert_eq!(
            page.source_text(),
            page.body,
            "a body opening with an H1 must not gain another title"
        );
        assert!(
            !escape_for_script_block(&page.source_text()).contains("ana@corp.com"),
            "escaping must not resurrect anything"
        );
    }
}
