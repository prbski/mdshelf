---
title: People Ops
allow:
  - hr@corp.com
deny:
  - team@corp.com
---

# People Ops

A **folder-level** rule: it governs `hr/`, everything beneath it, and this page.

Note the `deny`. An `allow` alone would only *add* `hr@corp.com` — the site-level grant
to `team@corp.com` would still reach in here. Naming them in `deny` is what keeps them
out.
