//! The Share control (S25, US-14 … US-16).
//!
//! Rendered here and handed to the theme as `{{ share_control }}`, rather than injected
//! into the page the way live-reload's script tag is. The theme decides *where* the
//! control appears and whether it appears at all; a theme that never mentions the
//! variable simply has no sharing, which is exactly what S25 asks for and what R5
//! documents as the cost.

use std::fmt::Write;

use crate::auth::store::LinkRecord;
use crate::links::pages::escape;
use crate::links::{LinkSettings, earliest_allowed_date, latest_allowed_date, time};

/// The lifetimes the popover offers as chips (S10).
///
/// `(value, label, spoken)`: the value is what the mint endpoint parses, the label is
/// what fits in a chip, and the spoken form is what a screen reader reads instead of
/// "5m". There is no month unit in [`crate::config::parse_duration`], so the longest
/// chip is spelled `30d` and labelled `1mo`.
///
/// Order is load-bearing: three per unit, so the three-column grid lays them out as a
/// row of minutes, a row of hours, a row of days, and a row of weeks and the month.
/// `max_lifetime` only ever trims the tail, so the grouping survives a capped site.
const PRESETS: [(&str, &str, &str); 12] = [
    ("5m", "5m", "5 minutes"),
    ("15m", "15m", "15 minutes"),
    ("30m", "30m", "30 minutes"),
    ("1h", "1h", "1 hour"),
    ("4h", "4h", "4 hours"),
    ("8h", "8h", "8 hours"),
    ("1d", "1d", "1 day"),
    ("2d", "2d", "2 days"),
    ("3d", "3d", "3 days"),
    ("1w", "1w", "1 week"),
    ("2w", "2w", "2 weeks"),
    ("30d", "1mo", "1 month"),
];

/// The idle label of the custom-date button. Kept in a data attribute so the script has
/// no second copy of it to drift from.
const CUSTOM_IDLE_LABEL: &str = "Custom date";

/// Render the control for one page.
///
/// `existing` is the set of live links this viewer already issued for this page, which
/// is what makes the popover answer "is this already shared?" where the question
/// actually arises (S12).
pub fn render(
    settings: &LinkSettings,
    page_url: &str,
    existing: &[LinkRecord],
    now: i64,
) -> String {
    let mut html = String::with_capacity(8192);
    let _ = write!(
        html,
        "<div class=\"mdshelf-share\" id=\"mdshelf-share\" data-page=\"{page}\">\n\
         <button type=\"button\" class=\"icon-button mdshelf-share-toggle\" \
         id=\"mdshelf-share-button\" aria-haspopup=\"dialog\" aria-expanded=\"false\" \
         aria-controls=\"mdshelf-share-panel\" title=\"Share\" aria-label=\"Share this page\">\
         <svg xmlns=\"http://www.w3.org/2000/svg\" width=\"16\" height=\"16\" \
         viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\" \
         stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\">\
         <circle cx=\"18\" cy=\"5\" r=\"3\"/><circle cx=\"6\" cy=\"12\" r=\"3\"/>\
         <circle cx=\"18\" cy=\"19\" r=\"3\"/><line x1=\"8.6\" y1=\"13.5\" x2=\"15.4\" y2=\"17.5\"/>\
         <line x1=\"15.4\" y1=\"6.5\" x2=\"8.6\" y2=\"10.5\"/></svg></button>\n\
         <div class=\"mdshelf-share-panel\" id=\"mdshelf-share-panel\" role=\"dialog\" \
         aria-label=\"Share this page\" aria-describedby=\"mdshelf-share-caption\" hidden>\n",
        page = escape(page_url)
    );

    html.push_str(
        "<div class=\"mdshelf-share-head\">\n\
         <p class=\"mdshelf-share-title\">Share this page</p>\n\
         <p class=\"mdshelf-share-caption\" id=\"mdshelf-share-caption\">Anyone with the \
         link can read this page, without signing in.</p>\n\
         </div>\n",
    );

    html.push_str(
        "<form id=\"mdshelf-share-form\" novalidate>\n\
         <fieldset class=\"mdshelf-share-durations\">\n\
         <legend>Expires in</legend>\n\
         <div class=\"mdshelf-share-chips\">\n",
    );
    for (value, label, spoken, checked) in duration_options(settings) {
        let _ = writeln!(
            html,
            "<label class=\"mdshelf-share-chip\">\
             <input type=\"radio\" name=\"lifetime\" value=\"{value}\"{checked} \
             aria-label=\"{spoken}\"><span>{label}</span></label>",
            value = escape(&value),
            checked = if checked { " checked" } else { "" },
            spoken = escape(&spoken),
            label = escape(&label)
        );
    }
    html.push_str("</div>\n");

    let _ = write!(
        html,
        "<button type=\"button\" class=\"mdshelf-share-custom\" id=\"mdshelf-share-custom\" \
         aria-pressed=\"false\" aria-expanded=\"false\" aria-controls=\"mdshelf-share-date\" \
         data-idle=\"{idle}\">\
         <svg xmlns=\"http://www.w3.org/2000/svg\" width=\"15\" height=\"15\" \
         viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\" \
         stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\">\
         <rect width=\"18\" height=\"18\" x=\"3\" y=\"4\" rx=\"2\"/>\
         <path d=\"M8 2v4M16 2v4M3 10h18\"/></svg>\
         <span class=\"mdshelf-share-custom-label\" id=\"mdshelf-share-custom-label\">\
         {idle}</span></button>\n\
         <input type=\"date\" class=\"mdshelf-share-date\" id=\"mdshelf-share-date\" \
         name=\"until\" min=\"{min}\" max=\"{max}\" aria-label=\"Custom expiry date\" hidden>\n\
         </fieldset>\n\
         <button type=\"submit\" class=\"mdshelf-share-create\" id=\"mdshelf-share-create\">\
         Create link</button>\n\
         <p class=\"mdshelf-share-error\" id=\"mdshelf-share-error\" role=\"alert\" hidden></p>\n\
         </form>\n",
        idle = escape(CUSTOM_IDLE_LABEL),
        min = escape(&earliest_allowed_date(now)),
        max = escape(&latest_allowed_date(settings, now))
    );

    html.push_str(
        "<div class=\"mdshelf-share-result\" id=\"mdshelf-share-result\" hidden>\n\
         <div class=\"mdshelf-share-url-row\">\n\
         <input type=\"text\" id=\"mdshelf-share-url\" readonly spellcheck=\"false\" \
         aria-label=\"Share URL\">\n\
         <button type=\"button\" class=\"mdshelf-share-copy\" id=\"mdshelf-share-copy\">\
         Copy</button>\n\
         </div>\n\
         <p class=\"mdshelf-share-once\">Shown once. mdshelf stores only a hash, so this \
         URL cannot be shown again &mdash; if it is lost, create another.</p>\n\
         </div>\n",
    );

    // No whitespace inside the list: an empty `<ul>` has to be *really* empty for
    // `:empty` to hide it, or an unshared page draws its separator and the empty
    // notice draws a second one right under it.
    let _ = write!(
        html,
        "<ul class=\"mdshelf-share-list\" id=\"mdshelf-share-list\">{}</ul>\n",
        existing
            .iter()
            .map(|record| list_row(record, now))
            .collect::<String>()
    );
    let _ = writeln!(
        html,
        "<p class=\"mdshelf-share-empty\" id=\"mdshelf-share-empty\"{hidden}>\
         No live links for this page.</p>",
        hidden = if existing.is_empty() { "" } else { " hidden" }
    );

    html.push_str("</div>\n</div>\n");
    html.push_str(STYLES);
    html.push_str(SCRIPT);
    html
}

/// One row of the per-page listing (US-16).
pub fn list_row(record: &LinkRecord, now: i64) -> String {
    format!(
        "<li data-link-id=\"{id}\"><span class=\"mdshelf-share-id\">{id}</span>\
         <span class=\"mdshelf-share-expiry\">expires in {expires_in}</span>\
         <button type=\"button\" class=\"mdshelf-share-revoke\" data-revoke=\"{id}\">Revoke</button></li>\n",
        id = escape(&record.id),
        expires_in = escape(&time::humanize_remaining(record.expires_at - now))
    )
}

/// The preset chips, with the one matching `default_lifetime` selected (US-15).
///
/// A preset the site's `max_lifetime` would refuse is not offered: a chip that always
/// answers with an error is worse than no chip. When `default_lifetime` is not one of
/// the presets it becomes its own chip rather than being rounded to a neighbour —
/// preselecting something the operator did not configure would quietly hand out links
/// of the wrong length.
fn duration_options(settings: &LinkSettings) -> Vec<(String, String, String, bool)> {
    let mut options: Vec<(String, String, String, bool)> = PRESETS
        .iter()
        .filter(|(value, _, _)| within_cap(value, settings))
        .map(|(value, label, spoken)| {
            (
                value.to_string(),
                label.to_string(),
                spoken.to_string(),
                is_default(value, settings),
            )
        })
        .collect();
    if let Some((value, label, spoken)) = custom_default(settings) {
        options.push((value, label, spoken, true));
    }
    options
}

/// Whether a preset is the configured default.
fn is_default(value: &str, settings: &LinkSettings) -> bool {
    crate::config::parse_duration(value)
        .map(|duration| duration == settings.default_lifetime)
        .unwrap_or(false)
}

/// Whether a preset is short enough for the site's `max_lifetime` to allow.
fn within_cap(value: &str, settings: &LinkSettings) -> bool {
    crate::config::parse_duration(value)
        .map(|duration| duration <= settings.max_lifetime)
        .unwrap_or(false)
}

/// The configured default as its own chip, when it is not one of the presets.
fn custom_default(settings: &LinkSettings) -> Option<(String, String, String)> {
    if PRESETS
        .iter()
        .any(|(value, _, _)| is_default(value, settings))
    {
        return None;
    }
    let seconds = settings.default_lifetime.as_secs();
    let (value, count, unit) = if seconds.is_multiple_of(86_400) {
        (format!("{}d", seconds / 86_400), seconds / 86_400, "day")
    } else if seconds.is_multiple_of(3_600) {
        (format!("{}h", seconds / 3_600), seconds / 3_600, "hour")
    } else if seconds.is_multiple_of(60) {
        (format!("{}m", seconds / 60), seconds / 60, "minute")
    } else {
        (format!("{seconds}s"), seconds, "second")
    };
    let plural = if count == 1 { "" } else { "s" };
    let spoken = format!("{count} {unit}{plural}");
    Some((value.clone(), value, spoken))
}

const STYLES: &str = r#"<style>
.mdshelf-share{position:relative;display:flex;align-items:center}
.mdshelf-share-toggle[aria-expanded="true"]{background:var(--surface-hover,#f4f4f5);
color:var(--text-heading,#09090b)}
.mdshelf-share-panel{position:absolute;right:0;top:calc(100% + .625rem);z-index:200;
width:20.5rem;max-width:calc(100vw - 1.5rem);display:flex;flex-direction:column;gap:.875rem;
padding:1rem;background:var(--bg,#fff);border:1px solid var(--border,#e4e4e7);
border-radius:var(--radius-sm,10px);box-shadow:var(--shadow-lg,0 12px 32px rgba(0,0,0,.14));
font-family:var(--font,system-ui,sans-serif);font-size:.8125rem;line-height:1.45;
color:var(--text,#3f3f46);text-align:left;animation:mdshelf-share-in .14s ease-out}
.mdshelf-share-panel[hidden]{display:none}
@keyframes mdshelf-share-in{from{opacity:0;transform:translateY(-.25rem)}to{opacity:1;transform:none}}
@media (prefers-reduced-motion:reduce){.mdshelf-share-panel{animation:none}}
.mdshelf-share-title{margin:0;font-size:.8125rem;font-weight:600;color:var(--text-heading,#09090b)}
.mdshelf-share-caption{margin:.1875rem 0 0;color:var(--muted,#71717a);font-size:.75rem}
#mdshelf-share-form{display:flex;flex-direction:column;gap:.75rem;margin:0}
.mdshelf-share-durations{display:block;margin:0;padding:0;border:0}
.mdshelf-share-durations legend{display:block;float:left;width:100%;margin:0 0 .5rem;padding:0;
font-size:.6875rem;font-weight:600;letter-spacing:.05em;text-transform:uppercase;
color:var(--muted,#71717a)}
/* One row per unit — minutes, hours, days, then weeks and the month — so the grid
   reads as four groups of three rather than twelve numbers. */
.mdshelf-share-chips{clear:both;display:grid;grid-template-columns:repeat(3,minmax(0,1fr));gap:.375rem}
.mdshelf-share-chip{position:relative;display:block}
.mdshelf-share-chip input{position:absolute;inset:0;width:100%;height:100%;margin:0;
opacity:0;appearance:none;cursor:pointer}
.mdshelf-share-chip span{display:block;padding:.375rem 0;border:1px solid var(--border,#e4e4e7);
border-radius:7px;background:var(--bg,#fff);color:var(--text,#3f3f46);font-size:.75rem;
font-weight:500;font-variant-numeric:tabular-nums;text-align:center;
transition:color .15s ease,background .15s ease,border-color .15s ease}
.mdshelf-share-chip:hover span{background:var(--surface-hover,#f4f4f5);color:var(--text-heading,#09090b)}
.mdshelf-share-chip input:checked+span{background:var(--accent-soft,rgba(5,150,105,.08));
border-color:var(--accent,#059669);color:var(--accent,#059669);font-weight:600}
.mdshelf-share-chip input:focus-visible+span{outline:2px solid var(--accent,#059669);outline-offset:1px}
.mdshelf-share-custom{display:flex;align-items:center;justify-content:center;gap:.5rem;
width:100%;min-height:2.25rem;margin-top:.5rem;padding:.5rem .75rem;
border:1px solid var(--border,#e4e4e7);border-radius:8px;background:var(--surface,#fafafa);
color:var(--text,#3f3f46);font:inherit;font-weight:500;cursor:pointer;
transition:color .15s ease,background .15s ease,border-color .15s ease}
.mdshelf-share-custom:hover{background:var(--surface-hover,#f4f4f5);color:var(--text-heading,#09090b)}
.mdshelf-share-custom:focus-visible{outline:2px solid var(--accent,#059669);outline-offset:1px}
.mdshelf-share-custom[aria-pressed="true"]{background:var(--accent-soft,rgba(5,150,105,.08));
border-color:var(--accent,#059669);color:var(--accent,#059669);font-weight:600}
.mdshelf-share-custom svg{flex:none}
.mdshelf-share-date{width:100%;margin-top:.5rem;padding:.4375rem .5rem;
border:1px solid var(--border,#e4e4e7);border-radius:8px;background:var(--bg,#fff);
color:inherit;font:inherit}
.mdshelf-share-date[hidden]{display:none}
.mdshelf-share-create{min-height:2.25rem;padding:.5rem .75rem;border:1px solid transparent;
border-radius:8px;background:var(--accent,#059669);color:#fff;font:inherit;font-weight:600;
cursor:pointer;transition:background .15s ease,opacity .15s ease}
.mdshelf-share-create:hover:not(:disabled){background:var(--accent-hover,#047857)}
.mdshelf-share-create:disabled{opacity:.6;cursor:default}
.mdshelf-share-error{margin:0;color:#dc2626;font-size:.75rem}
.mdshelf-share-error[hidden]{display:none}
.mdshelf-share-result{display:flex;flex-direction:column;gap:.375rem;padding:.75rem;
border:1px solid var(--border,#e4e4e7);border-radius:8px;background:var(--surface,#fafafa)}
.mdshelf-share-result[hidden]{display:none}
.mdshelf-share-url-row{display:flex;gap:.375rem;align-items:center}
#mdshelf-share-url{flex:1 1 auto;min-width:0;padding:.375rem .5rem;border-radius:6px;
border:1px solid var(--border,#e4e4e7);background:var(--bg,#fff);color:var(--text,#3f3f46);
font-family:var(--mono,ui-monospace,SFMono-Regular,Menlo,monospace);font-size:.75rem}
.mdshelf-share-copy{flex:none;padding:.375rem .625rem;border:1px solid var(--border,#e4e4e7);
border-radius:6px;background:var(--bg,#fff);color:var(--text,#3f3f46);font:inherit;
font-size:.75rem;font-weight:600;cursor:pointer;transition:color .15s ease,background .15s ease}
.mdshelf-share-copy:hover{background:var(--surface-hover,#f4f4f5);color:var(--text-heading,#09090b)}
.mdshelf-share-copy.is-done{border-color:var(--accent,#059669);color:var(--accent,#059669)}
.mdshelf-share-once{margin:0;color:var(--muted,#71717a);font-size:.6875rem;line-height:1.4}
.mdshelf-share-list{list-style:none;display:flex;flex-direction:column;gap:.25rem;
margin:0;padding:.875rem 0 0;border-top:1px solid var(--border,#e4e4e7)}
.mdshelf-share-list:empty{display:none}
.mdshelf-share-list li{display:flex;gap:.5rem;align-items:center}
.mdshelf-share-id{flex:none;padding:.125rem .375rem;border-radius:5px;
background:var(--surface-hover,#f4f4f5);
font-family:var(--mono,ui-monospace,SFMono-Regular,Menlo,monospace);font-size:.6875rem;
color:var(--text-heading,#09090b)}
.mdshelf-share-expiry{flex:1 1 auto;min-width:0;color:var(--muted,#71717a);font-size:.75rem}
.mdshelf-share-revoke{flex:none;padding:.1875rem .5rem;border:1px solid var(--border,#e4e4e7);
border-radius:6px;background:transparent;color:var(--muted,#71717a);font:inherit;
font-size:.6875rem;font-weight:600;cursor:pointer;
transition:color .15s ease,background .15s ease,border-color .15s ease}
.mdshelf-share-revoke:hover:not(:disabled){border-color:#dc2626;color:#dc2626}
.mdshelf-share-revoke:disabled{opacity:.6;cursor:default}
.mdshelf-share-empty{margin:0;padding-top:.875rem;border-top:1px solid var(--border,#e4e4e7);
color:var(--muted,#71717a);font-size:.75rem}
.mdshelf-share-empty[hidden]{display:none}
</style>
"#;

const SCRIPT: &str = r#"<script>
(function () {
  var root = document.getElementById('mdshelf-share');
  if (!root) return;
  var button = document.getElementById('mdshelf-share-button');
  var panel = document.getElementById('mdshelf-share-panel');
  var form = document.getElementById('mdshelf-share-form');
  var custom = document.getElementById('mdshelf-share-custom');
  var customLabel = document.getElementById('mdshelf-share-custom-label');
  var date = document.getElementById('mdshelf-share-date');
  var create = document.getElementById('mdshelf-share-create');
  var error = document.getElementById('mdshelf-share-error');
  var result = document.getElementById('mdshelf-share-result');
  var urlField = document.getElementById('mdshelf-share-url');
  var copy = document.getElementById('mdshelf-share-copy');
  var list = document.getElementById('mdshelf-share-list');
  var empty = document.getElementById('mdshelf-share-empty');

  function open(next, restoreFocus) {
    panel.hidden = !next;
    button.setAttribute('aria-expanded', String(next));
    if (next) {
      var checked = form.querySelector('input[name="lifetime"]:checked');
      (checked || custom).focus();
    } else if (restoreFocus) {
      button.focus();
    }
  }
  button.addEventListener('click', function () { open(panel.hidden, true); });
  document.addEventListener('click', function (event) {
    if (!panel.hidden && !root.contains(event.target)) open(false, false);
  });
  document.addEventListener('keydown', function (event) {
    if (event.key === 'Escape' && !panel.hidden) open(false, true);
  });

  function fail(message) { error.textContent = message; error.hidden = false; }

  function refreshEmpty() { empty.hidden = list.children.length > 0; }

  // The custom button is the thirteenth option, not a thirteenth chip: pressing it
  // clears the preset selection, so exactly one lifetime is ever in effect.
  function useCustom(on) {
    custom.setAttribute('aria-pressed', String(on));
    if (!on) return;
    var chips = form.querySelectorAll('input[name="lifetime"]');
    Array.prototype.forEach.call(chips, function (chip) { chip.checked = false; });
  }
  function customIsChosen() { return custom.getAttribute('aria-pressed') === 'true'; }

  // Formatted as UTC, because a bare date means the end of that day in UTC on the
  // server; formatting it locally could show the day either side of the real expiry.
  function humanDate(value) {
    var parts = value.split('-');
    var stamp = Date.UTC(Number(parts[0]), Number(parts[1]) - 1, Number(parts[2]));
    try {
      return new Date(stamp).toLocaleDateString(undefined, {
        timeZone: 'UTC', year: 'numeric', month: 'short', day: 'numeric'
      });
    } catch (e) { return value; }
  }

  custom.addEventListener('click', function () {
    date.hidden = false;
    custom.setAttribute('aria-expanded', 'true');
    useCustom(true);
    date.focus();
    if (typeof date.showPicker === 'function') { try { date.showPicker(); } catch (e) {} }
  });
  date.addEventListener('change', function () {
    if (!date.value) {
      customLabel.textContent = custom.getAttribute('data-idle');
      useCustom(false);
      return;
    }
    customLabel.textContent = custom.getAttribute('data-idle') + ' \u00b7 ' + humanDate(date.value);
    useCustom(true);
  });
  // Picking a preset puts the custom field away again: leaving a date input open under
  // a selected chip suggests both are in effect, and only one ever is.
  form.addEventListener('change', function (event) {
    if (!event.target || event.target.name !== 'lifetime') return;
    useCustom(false);
    date.hidden = true;
    custom.setAttribute('aria-expanded', 'false');
  });

  function wireRevoke(node) {
    var control = node.querySelector('[data-revoke]');
    if (!control) return;
    control.addEventListener('click', function () {
      control.disabled = true;
      fetch('/__share/revoke', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ id: control.getAttribute('data-revoke') })
      }).then(function (response) {
        if (!response.ok) { control.disabled = false; fail('Could not revoke that link.'); return; }
        node.remove();
        refreshEmpty();
      }).catch(function () { control.disabled = false; fail('Could not revoke that link.'); });
    });
  }
  Array.prototype.forEach.call(list.children, wireRevoke);

  form.addEventListener('submit', function (event) {
    event.preventDefault();
    error.hidden = true;
    var body = { url: root.getAttribute('data-page') };
    if (customIsChosen()) {
      if (!date.value) { date.hidden = false; fail('Pick a date.'); date.focus(); return; }
      if (date.max && date.value > date.max) {
        fail('That is beyond the longest link this site allows (' + date.max + ').');
        return;
      }
      if (date.min && date.value < date.min) { fail('Pick a date that is still ahead.'); return; }
      body.until = date.value;
    } else {
      var choice = form.querySelector('input[name="lifetime"]:checked');
      if (choice) body['for'] = choice.value;
    }
    create.disabled = true;
    fetch('/__share', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body)
    }).then(function (response) {
      return response.json().then(function (payload) { return { ok: response.ok, payload: payload }; });
    }).then(function (outcome) {
      create.disabled = false;
      if (!outcome.ok) { fail(outcome.payload.error || 'Could not create a link.'); return; }
      urlField.value = outcome.payload.url;
      result.hidden = false;
      urlField.focus();
      urlField.select();
      var row = document.createElement('li');
      row.setAttribute('data-link-id', outcome.payload.id);
      row.innerHTML = '<span class="mdshelf-share-id"></span>' +
        '<span class="mdshelf-share-expiry"></span>' +
        '<button type="button" class="mdshelf-share-revoke">Revoke</button>';
      row.querySelector('.mdshelf-share-id').textContent = outcome.payload.id;
      row.querySelector('.mdshelf-share-expiry').textContent = 'expires in ' + outcome.payload.expires_in;
      row.querySelector('.mdshelf-share-revoke').setAttribute('data-revoke', outcome.payload.id);
      list.appendChild(row);
      wireRevoke(row);
      refreshEmpty();
    }).catch(function () { create.disabled = false; fail('Could not create a link.'); });
  });

  copy.addEventListener('click', function () {
    urlField.select();
    if (navigator.clipboard) navigator.clipboard.writeText(urlField.value);
    else document.execCommand('copy');
    copy.textContent = 'Copied';
    copy.classList.add('is-done');
    setTimeout(function () { copy.textContent = 'Copy'; copy.classList.remove('is-done'); }, 1500);
  });
})();
</script>
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn settings(default_lifetime: Duration) -> LinkSettings {
        LinkSettings {
            default_lifetime,
            ..LinkSettings::default()
        }
    }

    #[test]
    fn every_preset_is_offered_as_a_chip() {
        let html = render(&LinkSettings::default(), "/docs/a", &[], 0);
        for (value, label, spoken) in PRESETS {
            assert!(
                html.contains(&format!("value=\"{value}\"")),
                "missing {value}"
            );
            assert!(
                html.contains(&format!("<span>{label}</span>")),
                "missing {label}"
            );
            assert!(
                html.contains(&format!("aria-label=\"{spoken}\"")),
                "missing {spoken}"
            );
        }
    }

    /// The custom expiry is one bigger button plus the date field it reveals, not a
    /// thirteenth chip.
    #[test]
    fn the_custom_date_is_its_own_button_and_reveals_the_date_field() {
        let html = render(&LinkSettings::default(), "/docs/a", &[], 0);
        assert!(html.contains("id=\"mdshelf-share-custom\""), "got: {html}");
        assert!(html.contains("aria-controls=\"mdshelf-share-date\""));
        assert!(html.contains("data-idle=\"Custom date\""));
        assert!(html.contains("type=\"date\""), "the until field");
        // Hidden until the button asks for it, so the resting popover is just chips.
        assert!(
            html.contains("aria-label=\"Custom expiry date\" hidden"),
            "got: {html}"
        );
        // And it is no longer a radio the form can submit on its own.
        assert!(!html.contains("value=\"until\""), "got: {html}");
    }

    /// US-15: the preselected option matches `default_lifetime`.
    #[test]
    fn the_configured_default_is_the_preselected_option() {
        let html = render(&settings(Duration::from_secs(3_600)), "/docs/a", &[], 0);
        assert!(html.contains("value=\"1h\" checked"), "got: {html}");
        assert!(!html.contains("value=\"1d\" checked"));

        let html = render(
            &settings(Duration::from_secs(7 * 86_400)),
            "/docs/a",
            &[],
            0,
        );
        assert!(html.contains("value=\"1w\" checked"), "got: {html}");

        let html = render(&settings(Duration::from_secs(900)), "/docs/a", &[], 0);
        assert!(html.contains("value=\"15m\" checked"), "got: {html}");
    }

    /// A default that is not one of the presets is offered as its own chip rather than
    /// rounded to a neighbour.
    #[test]
    fn an_unusual_default_becomes_an_extra_chip() {
        let html = render(&settings(Duration::from_secs(6 * 3_600)), "/docs/a", &[], 0);
        assert!(html.contains("value=\"6h\" checked"), "got: {html}");
        assert!(html.contains("aria-label=\"6 hours\""));
        assert!(!html.contains("value=\"8h\" checked"));

        let html = render(&settings(Duration::from_secs(600)), "/docs/a", &[], 0);
        assert!(html.contains("value=\"10m\" checked"), "got: {html}");
        assert!(html.contains("aria-label=\"10 minutes\""));
    }

    /// A chip the server would refuse is not offered: `max_lifetime` bounds the row of
    /// presets exactly as it bounds the date field.
    #[test]
    fn presets_beyond_the_cap_are_not_offered() {
        let capped = LinkSettings {
            max_lifetime: Duration::from_secs(7 * 86_400),
            ..LinkSettings::default()
        };
        let html = render(&capped, "/docs/a", &[], 0);
        assert!(
            html.contains("value=\"1w\""),
            "the cap itself stays: {html}"
        );
        assert!(!html.contains("value=\"2w\""), "got: {html}");
        assert!(!html.contains("value=\"30d\""), "got: {html}");
    }

    /// An unshared page draws exactly one separator: the empty `<ul>` has to carry no
    /// text node at all, or `:empty` misses it and the notice underneath adds a second
    /// rule right below the first.
    #[test]
    fn an_unshared_page_renders_a_truly_empty_list() {
        let html = render(&LinkSettings::default(), "/docs/a", &[], 0);
        assert!(
            html.contains("id=\"mdshelf-share-list\"></ul>"),
            "got: {html}"
        );
        assert!(html.contains("id=\"mdshelf-share-empty\">"), "got: {html}");
    }

    #[test]
    fn the_date_field_carries_the_bounds_the_server_enforces() {
        let settings = LinkSettings::default();
        let now = 1_787_000_000_000i64;
        let html = render(&settings, "/docs/a", &[], now);
        let latest = latest_allowed_date(&settings, now);
        let earliest = earliest_allowed_date(now);
        assert!(html.contains(&format!("max=\"{latest}\"")), "got: {html}");
        assert!(html.contains(&format!("min=\"{earliest}\"")), "got: {html}");
    }

    #[test]
    fn existing_links_are_listed_with_a_revoke_control() {
        let now = 1_787_000_000_000i64;
        let record = LinkRecord {
            id: "ab12cd".into(),
            site: "/vault".into(),
            path: "a.md".into(),
            expires_at: now + 3_600_000,
            created_at: now,
            issued_by: "ana@corp.com".into(),
            revoked_at: None,
        };
        let html = render(&LinkSettings::default(), "/docs/a", &[record], now);
        assert!(html.contains("ab12cd"));
        assert!(html.contains("data-revoke=\"ab12cd\""));
        assert!(html.contains("expires in 1 hour"));
        assert!(
            html.contains("id=\"mdshelf-share-empty\" hidden"),
            "got: {html}"
        );
    }

    #[test]
    fn the_page_url_is_escaped_into_the_markup() {
        let html = render(
            &LinkSettings::default(),
            "/docs/\"><script>alert(1)</script>",
            &[],
            0,
        );
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(html.contains("&lt;script&gt;"));
    }
}
