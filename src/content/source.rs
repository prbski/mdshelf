use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::warn;
use walkdir::{DirEntry, WalkDir};

/// Iterate Markdown source files within a site root, skipping common noise.
pub fn iter_markdown_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry_result in WalkDir::new(root)
        .follow_links(true)
        .into_iter()
        .filter_entry(|entry| !is_noise(entry))
    {
        let entry = match entry_result {
            Ok(entry) => entry,
            Err(walk_error) => {
                warn!(
                    error = %walk_error,
                    root = %root.display(),
                    "skipping path while scanning site (missing target, broken symlink, or permission issue)"
                );
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        if !is_markdown(entry.path()) {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(root)
            .with_context(|| format!("stripping prefix {}", root.display()))?;
        if relative_path_has_hidden_component(rel.as_ref()) {
            continue;
        }
        out.push(rel.to_path_buf());
    }
    out.sort();
    Ok(out)
}

/// True if any normal path segment starts with `.` (e.g. `.git`, `.cursor`).
/// Used so symlinked trees cannot pull tool or VCS folders into the site.
pub(super) fn relative_path_has_hidden_component(relative: &Path) -> bool {
    for component in relative.components() {
        if let std::path::Component::Normal(part) = component {
            if part
                .to_str()
                .is_some_and(|segment| segment.starts_with('.'))
            {
                return true;
            }
        }
    }
    false
}

fn is_noise(entry: &DirEntry) -> bool {
    let name = entry.file_name().to_string_lossy();
    if entry.depth() == 0 {
        return false;
    }
    if name.starts_with('.') {
        return true;
    }
    matches!(
        name.as_ref(),
        "node_modules" | "target" | "dist" | "build" | "__pycache__"
    )
}

fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown"))
        .unwrap_or(false)
}
