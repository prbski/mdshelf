---
title: Welcome to mdshelf
description: A fast, themeable Rust server for folders of markdown files.
layout: doc
sidebar_order: 1
---

# Welcome to mdshelf

You are reading a page rendered from a single Markdown file. Drop more `.md`
files next to this one — or create subfolders — and they will appear in the
sidebar automatically, mirroring the folder hierarchy on disk.

## Highlights

- **Live reload**: edit a file, see the page update without a manual refresh.
- **Mobile-first reading**: sidebar and table of contents collapse into off-canvas
  drawers on small screens so reading content stays the priority.
- **Themes**: layered theme directories, frontmatter, layouts, partials.
- **CLI or system service**: run `mdshelf serve` ad-hoc, or `mdshelf install` to
  register a launchd / systemd / Windows service that runs in the background.

## Markdown features

You can use everything from GitHub-Flavored Markdown:

```rust
fn main() {
    println!("Hello from a syntax-highlighted block!");
}
```

| Feature             | Supported |
| ------------------- | --------- |
| Tables              | yes       |
| Task lists          | yes       |
| Footnotes           | yes       |
| Autolink headings   | yes       |

> Tip: each Markdown file may declare frontmatter (the YAML block at the top of
> this file) to customize its title, layout, sidebar order, and more.

Happy writing.
