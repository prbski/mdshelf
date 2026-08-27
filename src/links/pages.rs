//! The two pages a link reader can receive: the deny page and the reading view shell.
//!
//! Both are self-contained HTML rather than theme templates, for the same reason
//! [`crate::auth::pages`] is: a partially-customised theme cannot break them, and their
//! output cannot vary with what was asked for. That invariance is the whole of SEC-3 —
//! expired, revoked, unknown, malformed and nonexistent must be byte-identical, and the
//! cheapest way to guarantee that is a function that takes no arguments.

/// The deny page for every dead, unknown, malformed or nonexistent link (S13/US-9).
///
/// Takes no arguments on purpose. There is no path, no title, no site name and no
/// issuer it *could* mention, so a stale URL in somebody's browser history reveals
/// nothing — not even that its target ever existed.
pub fn denied() -> &'static str {
    DENIED_PAGE
}

const DENIED_PAGE: &str = concat!(
    "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n",
    "<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n",
    "<meta name=\"robots\" content=\"noindex,nofollow\">\n",
    "<title>Link unavailable</title>\n<style>",
    ":root{color-scheme:light dark;--bg:#fafafa;--fg:#18181b;--muted:#52525b;--line:#e4e4e7;--card:#fff}",
    "@media(prefers-color-scheme:dark){:root{--bg:#09090b;--fg:#fafafa;--muted:#a1a1aa;--line:#27272a;--card:#18181b}}",
    "*{box-sizing:border-box}",
    "body{margin:0;min-height:100vh;display:grid;place-items:center;padding:1.5rem;background:var(--bg);color:var(--fg);",
    "font:16px/1.6 ui-sans-serif,system-ui,-apple-system,\"Segoe UI\",Roboto,sans-serif;-webkit-font-smoothing:antialiased}",
    "main{width:100%;max-width:26rem;background:var(--card);border:1px solid var(--line);border-radius:12px;padding:2rem}",
    "h1{margin:0 0 .5rem;font-size:1.25rem;font-weight:600;letter-spacing:-.01em}",
    "p{margin:0;color:var(--muted)}",
    "footer{margin-top:1.5rem;font-size:.8125rem;color:var(--muted)}",
    "</style>\n</head>\n<body>\n",
    "<main>\n<h1>This link is not available</h1>\n",
    "<p>It may have expired, it may have been turned off, or it may never have existed. ",
    "Ask whoever sent it to you for a new one.</p>\n",
    "<footer>Served by mdshelf</footer>\n</main>\n",
    "</body>\n</html>\n"
);

/// The reading view, for a theme that ships no `layouts/link.html` (R5).
///
/// Deliberately links the theme's own stylesheets rather than inlining a private copy,
/// so a recipient sees the site's colours and fonts (S31) even though the bundled
/// template is not in play. Those routes are served to everyone — they carry no vault
/// content — so a link reader can fetch them without a session.
pub fn reading_view_fallback(ctx: &crate::render::templates::LinkTemplateContext) -> String {
    let reload = if ctx.live_reload {
        format!("<script>{}</script>\n", ctx.reload_script)
    } else {
        String::new()
    };
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n\
         <meta name=\"robots\" content=\"noindex,nofollow\">\n\
         <meta name=\"referrer\" content=\"no-referrer\">\n\
         <title>{title}</title>\n\
         <link rel=\"stylesheet\" href=\"/__assets/vendor/inter/inter.css\">\n\
         <link rel=\"stylesheet\" href=\"/__mdshelf/syntax.css\">\n\
         <link rel=\"stylesheet\" href=\"/__assets/css/main.css\">\n\
         <link rel=\"stylesheet\" href=\"/__assets/css/syntax.css\">\n\
         <style>:root{{--accent:{color}}}\
         .link-shell{{max-width:48rem;margin:0 auto;padding:2rem 1.25rem 4rem}}\
         .link-banner{{margin:0 0 2rem;padding:.75rem 1rem;border:1px solid var(--border,#e4e4e7);\
         border-radius:10px;font-size:.875rem;line-height:1.5}}</style>\n\
         </head>\n<body class=\"mdshelf-body\">\n\
         <div class=\"link-shell\">\n\
         <p class=\"link-banner\">Shared by {issuer} &middot; expires in {expires_in}</p>\n\
         <article class=\"doc-article prose\">\n{html}\n</article>\n\
         </div>\n{reload}</body>\n</html>\n",
        title = escape(&ctx.page.title),
        color = escape(&ctx.site.color),
        issuer = escape(&ctx.banner.issuer),
        expires_in = escape(&ctx.banner.expires_in),
        html = ctx.page.html,
        reload = reload,
    )
}

/// The live-reload client for one shared page (S27).
///
/// Intentionally tiny and inline: the bundled `livereload.js` talks to `/__livereload`,
/// which requires a session a link reader does not have.
pub fn reload_script(reload_path: &str) -> String {
    format!(
        "(function(){{var p={path};var s=new WebSocket((location.protocol==='https:'?'wss://':'ws://')+location.host+p);\
         s.onmessage=function(e){{if(e.data==='reload')location.reload();}};}})();",
        path = serde_json::to_string(reload_path).unwrap_or_else(|_| "\"\"".to_string())
    )
}

/// Minimal HTML escaping for the few values interpolated above.
pub(crate) fn escape(raw: &str) -> String {
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

/// One row of the `Shared by you` page.
pub struct ShareRow {
    pub id: String,
    /// Where the page lives, as its reader would type it.
    pub page: String,
    pub expires_in: String,
}

/// The `Shared by you` listing (US-17/S12).
///
/// Answers "what am I currently exposing?" across every site, which is a different
/// question from the popover's "is this page already shared?" and deserves its own
/// page rather than a filter on somebody else's.
pub fn shares_page(email: &str, rows: &[ShareRow]) -> String {
    use std::fmt::Write;

    let mut body = String::new();
    let _ = write!(
        body,
        "<main>\n<h1>Shared by you</h1>\n\
         <p>Every live link you have created, across all sites. Anyone holding one of \
         these URLs can read that page without signing in, until it expires or you \
         revoke it &mdash; and each one dies on its own if you lose access to the page \
         it points at.</p>\n\
         <p class=\"who\">Signed in as {email}</p>\n",
        email = escape(email)
    );

    if rows.is_empty() {
        body.push_str("<p class=\"empty\">You have no live links.</p>\n");
    } else {
        body.push_str("<table>\n<thead><tr><th>Page</th><th>Expires</th><th>Link</th><th></th></tr></thead>\n<tbody>\n");
        for row in rows {
            let _ = writeln!(
                body,
                "<tr data-link-id=\"{id}\"><td>{page}</td><td>in {expires_in}</td>\
                 <td class=\"id\">{id}</td>\
                 <td><button type=\"button\" data-revoke=\"{id}\">Revoke</button></td></tr>",
                id = escape(&row.id),
                page = escape(&row.page),
                expires_in = escape(&row.expires_in)
            );
        }
        body.push_str("</tbody>\n</table>\n");
    }

    body.push_str("<footer>Served by mdshelf</footer>\n</main>\n");
    body.push_str(SHARES_SCRIPT);
    shares_shell("Shared by you", &body)
}

fn shares_shell(title: &str, body: &str) -> String {
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n\
         <meta name=\"robots\" content=\"noindex,nofollow\">\n\
         <title>{title}</title>\n\
         <link rel=\"stylesheet\" href=\"/__assets/vendor/inter/inter.css\">\n\
         <style>{SHARES_STYLES}</style>\n</head>\n<body>\n{body}\n</body>\n</html>\n"
    )
}

const SHARES_STYLES: &str = concat!(
    ":root{color-scheme:light dark;--bg:#fafafa;--fg:#18181b;--muted:#52525b;--line:#e4e4e7;--card:#fff}",
    "@media(prefers-color-scheme:dark){:root{--bg:#09090b;--fg:#fafafa;--muted:#a1a1aa;--line:#27272a;--card:#18181b}}",
    "*{box-sizing:border-box}",
    "body{margin:0;padding:2.5rem 1.5rem;background:var(--bg);color:var(--fg);",
    "font:16px/1.6 Inter,ui-sans-serif,system-ui,-apple-system,\"Segoe UI\",Roboto,sans-serif;-webkit-font-smoothing:antialiased}",
    "main{max-width:52rem;margin:0 auto;background:var(--card);border:1px solid var(--line);border-radius:12px;padding:2rem}",
    "h1{margin:0 0 .5rem;font-size:1.375rem;font-weight:600;letter-spacing:-.01em}",
    "p{margin:0 0 1rem;color:var(--muted)}",
    ".who{display:inline-block;padding:.375rem .625rem;border:1px solid var(--line);border-radius:8px;font-size:.875rem;color:var(--fg)}",
    "table{width:100%;border-collapse:collapse;margin-top:1.5rem;font-size:.9375rem}",
    "th{text-align:left;font-weight:600;color:var(--muted);font-size:.8125rem;text-transform:uppercase;letter-spacing:.04em}",
    "th,td{padding:.625rem .5rem;border-bottom:1px solid var(--line)}",
    "td.id{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;color:var(--muted)}",
    "button{border:1px solid var(--line);border-radius:6px;background:transparent;color:inherit;padding:.25rem .625rem;cursor:pointer}",
    "button:hover{border-color:var(--muted)}",
    "footer{margin-top:2rem;font-size:.8125rem;color:var(--muted)}"
);

const SHARES_SCRIPT: &str = r#"<script>
(function () {
  document.querySelectorAll('[data-revoke]').forEach(function (control) {
    control.addEventListener('click', function () {
      control.disabled = true;
      fetch('/__share/revoke', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ id: control.getAttribute('data-revoke') })
      }).then(function (response) {
        if (!response.ok) { control.disabled = false; return; }
        var row = control.closest('tr');
        if (row) row.remove();
      }).catch(function () { control.disabled = false; });
    });
  });
})();
</script>
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// SEC-3, stated as a property of the function's shape: there is no argument it
    /// could vary on.
    #[test]
    fn the_deny_page_is_one_fixed_string() {
        assert_eq!(denied(), denied());
        assert!(!denied().is_empty());
    }

    /// US-9: the body contains no path, title, site name or issuer.
    #[test]
    fn the_deny_page_names_nothing_about_the_request() {
        let page = denied();
        for leak in ["/docs", "/s/", "issued", "http://", "https://"] {
            assert!(
                !page.contains(leak),
                "the deny page must not contain {leak:?}"
            );
        }
        // No address, and no room for one to be added quietly later. Every `@` in the
        // page has to be a CSS at-rule.
        for (index, _) in page.match_indices('@') {
            let tail = &page[index + 1..];
            assert!(
                tail.starts_with("media"),
                "unexpected `@` in the deny page at byte {index}: {}",
                &tail[..tail.len().min(40)]
            );
        }
        assert!(page.contains("noindex"));
    }
}
