---
title: Compensation Review
deny:
  - contractor@corp.com
---

# Compensation Review

A **file-level** rule. `hr@corp.com` inherits access from the folder above; the `deny`
here excludes one address that would otherwise have it.

Run `mdshelf acl explain hr/compensation.md hr@corp.com` to see the resolution trace.
