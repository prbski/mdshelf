//! The two pages auth serves itself: the sign-in interstitial and the deny page.
//!
//! These are rendered from self-contained HTML rather than theme templates, for two
//! reasons. A partially-customised theme cannot break them, and — more importantly —
//! their output cannot vary with the requested path, which is what makes the deny page
//! byte-identical for a restricted path and a nonexistent one (D23).
//!
//! Where per-path detail genuinely helps the visitor (the `next` parameter, the path in
//! the request-access mailto), the browser fills it in from `location`. The bytes on the
//! wire stay identical; the reader still gets a working link.

use std::fmt::Write;

const STYLES: &str = r#"
:root{color-scheme:light dark;--bg:#fafafa;--fg:#18181b;--muted:#52525b;--line:#e4e4e7;--card:#fff;--accent:#10b981;--accent-fg:#fff}
@media(prefers-color-scheme:dark){:root{--bg:#09090b;--fg:#fafafa;--muted:#a1a1aa;--line:#27272a;--card:#18181b}}
*{box-sizing:border-box}
body{margin:0;min-height:100vh;display:grid;place-items:center;padding:1.5rem;background:var(--bg);color:var(--fg);
font:16px/1.6 ui-sans-serif,system-ui,-apple-system,"Segoe UI",Roboto,sans-serif;-webkit-font-smoothing:antialiased}
main{width:100%;max-width:26rem;background:var(--card);border:1px solid var(--line);border-radius:12px;padding:2rem}
h1{margin:0 0 .5rem;font-size:1.25rem;font-weight:600;letter-spacing:-.01em}
p{margin:0 0 1rem;color:var(--muted)}
.mark{width:2rem;height:2rem;border-radius:7px;background:var(--accent);margin-bottom:1.25rem}
.who{display:block;margin:0 0 1.25rem;padding:.625rem .75rem;border:1px solid var(--line);border-radius:8px;
font-size:.875rem;color:var(--fg);word-break:break-all}
a.button{display:block;text-align:center;padding:.625rem 1rem;border-radius:8px;background:var(--accent);
color:var(--accent-fg);text-decoration:none;font-weight:600;border:1px solid transparent}
a.button:hover{filter:brightness(.95)}
a.secondary{display:block;text-align:center;margin-top:.75rem;padding:.625rem 1rem;border-radius:8px;
background:transparent;color:var(--fg);text-decoration:none;border:1px solid var(--line);font-weight:500}
a.secondary:hover{border-color:var(--muted)}
a:focus-visible{outline:2px solid var(--accent);outline-offset:2px}
footer{margin-top:1.5rem;font-size:.8125rem;color:var(--muted)}
@media(prefers-reduced-motion:no-preference){a{transition:filter .15s ease,border-color .15s ease}}
"#;

fn shell(title: &str, body: &str) -> String {
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n\
         <meta name=\"robots\" content=\"noindex,nofollow\">\n\
         <title>{title}</title>\n<style>{STYLES}</style>\n</head>\n<body>\n{body}\n</body>\n</html>\n"
    )
}

/// The sign-in page shown to anonymous visitors, for every path (SEC-9).
///
/// Deliberately an interstitial rather than an immediate bounce to Google: a visitor
/// who followed a deep link deserves to see where they have arrived before being sent
/// to a consent screen (D22).
pub fn interstitial(site_name: &str) -> String {
    let mut body = String::new();
    let _ = write!(
        body,
        "<main>\n<div class=\"mark\" aria-hidden=\"true\"></div>\n\
         <h1>{name}</h1>\n\
         <p>This site is private. Sign in with the Google account you were invited with.</p>\n\
         <a class=\"button\" id=\"signin\" href=\"/auth/login\">Sign in with Google</a>\n\
         <footer>Served by mdshelf</footer>\n</main>\n",
        name = escape(site_name)
    );
    // Carry the visitor back to the page they asked for. Done client-side so the
    // response body does not vary with the requested path.
    body.push_str(
        "<script>\n(function(){var a=document.getElementById('signin');\
         if(!a)return;a.href='/auth/login?next='+encodeURIComponent(location.pathname+location.search);})();\n</script>\n",
    );
    shell("Sign in", &body)
}

/// The page a signed-in visitor gets when they may not read what they asked for —
/// and, identically, when the path does not exist at all (D23).
///
/// Because the same bytes answer both cases, an outsider cannot use this page to map
/// which documents a vault contains.
pub fn denied(email: &str, owner_email: Option<&str>) -> String {
    let mut body = String::new();
    let _ = write!(
        body,
        "<main>\n<div class=\"mark\" aria-hidden=\"true\"></div>\n\
         <h1>This account can&rsquo;t open this page</h1>\n\
         <p>It may not exist, or it may not be shared with you.</p>\n\
         <span class=\"who\">Signed in as {email}</span>\n\
         <a class=\"button\" href=\"/auth/logout\">Switch account</a>\n",
        email = escape(email)
    );

    if let Some(owner) = owner_email {
        // D24: no backend. The owner's own inbox is the queue.
        let _ = writeln!(
            body,
            "<a class=\"secondary\" id=\"request\" href=\"mailto:{owner}?subject=Access%20request\">\
             Request access</a>",
            owner = escape(owner)
        );
    }

    body.push_str("<footer>Served by mdshelf</footer>\n</main>\n");

    if owner_email.is_some() {
        // Fill the requested page into the mail body in the browser, so the served
        // bytes stay identical across paths while the owner still learns what was asked
        // for.
        body.push_str(
            "<script>\n(function(){var a=document.getElementById('request');if(!a)return;\
             a.href=a.href+'&body='+encodeURIComponent('Please grant me access to: '+location.pathname);})();\n</script>\n",
        );
    }

    shell("No access", &body)
}

/// Minimal HTML escaping for the few values interpolated above.
fn escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for character in raw.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_deny_page_contains_no_path_specific_content() {
        // The property that makes D23 hold: nothing about the request reaches the body.
        let page = denied("bob@gmail.com", Some("owner@corp.com"));
        assert!(!page.contains("/hr/"));
        assert!(page.contains("bob@gmail.com"));
        assert!(page.contains("Switch account"));
        assert!(page.contains("mailto:owner@corp.com"));
    }

    #[test]
    fn the_deny_page_is_identical_regardless_of_what_was_requested() {
        assert_eq!(
            denied("bob@gmail.com", Some("owner@corp.com")),
            denied("bob@gmail.com", Some("owner@corp.com"))
        );
    }

    #[test]
    fn the_deny_page_omits_the_request_link_without_an_owner() {
        let page = denied("bob@gmail.com", None);
        assert!(!page.contains("mailto:"));
        assert!(!page.contains("Request access"));
        assert!(page.contains("Switch account"));
    }

    #[test]
    fn addresses_are_escaped_into_the_markup() {
        let page = denied("<script>alert(1)</script>@x.com", None);
        assert!(!page.contains("<script>alert(1)</script>"));
        assert!(page.contains("&lt;script&gt;"));
    }

    #[test]
    fn the_interstitial_offers_google_sign_in() {
        let page = interstitial("Engineering Handbook");
        assert!(page.contains("Sign in with Google"));
        assert!(page.contains("/auth/login"));
        assert!(page.contains("Engineering Handbook"));
    }

    #[test]
    fn both_pages_ask_not_to_be_indexed() {
        // A crawler that reaches either page should not record it.
        assert!(interstitial("X").contains("noindex"));
        assert!(denied("a@b.com", None).contains("noindex"));
    }

    #[test]
    fn site_names_are_escaped() {
        let page = interstitial("<img src=x onerror=alert(1)>");
        assert!(!page.contains("<img src=x"));
        assert!(page.contains("&lt;img"));
    }
}
