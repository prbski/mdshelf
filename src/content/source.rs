use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use notify::EventKind;
use notify::event::ModifyKind;
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

/// Non-Markdown files in a site root (images, fonts, etc.) for static export.
pub fn iter_site_static_files(root: &Path) -> Result<Vec<PathBuf>> {
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
                    "skipping path while scanning static files"
                );
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        if is_markdown(entry.path()) {
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
pub fn relative_path_has_hidden_component(relative: &Path) -> bool {
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
    if entry.depth() == 0 {
        return false;
    }
    path_segment_is_noise(entry.file_name())
}

/// True when a single path segment is a dot-folder or common build/cache directory.
pub fn path_segment_is_noise(segment: &std::ffi::OsStr) -> bool {
    let name = segment.to_string_lossy();
    if name.starts_with('.') {
        return true;
    }
    matches!(
        name.as_ref(),
        "node_modules" | "target" | "dist" | "build" | "__pycache__"
    )
}

/// True when any path segment is noise (e.g. `.git`, `target`, `node_modules`).
pub fn path_has_noise_component(path: &Path) -> bool {
    path.components().any(|component| {
        matches!(component, std::path::Component::Normal(segment) if path_segment_is_noise(segment))
    })
}

pub fn is_markdown_path(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown"))
        .unwrap_or(false)
}

fn is_markdown(path: &Path) -> bool {
    is_markdown_path(path)
}

/// True for theme template and asset files that should trigger a rebuild.
pub fn is_theme_watch_path(path: &Path) -> bool {
    let path_text = path.to_string_lossy().replace('\\', "/");
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    if path_text.contains("/layouts/") || path_text.contains("/partials/") {
        return matches!(extension, "html" | "xml" | "txt");
    }
    if path_text.contains("/assets/") {
        return matches!(
            extension,
            "css" | "js" | "woff" | "woff2" | "svg" | "png" | "jpg" | "jpeg" | "gif" | "webp"
        );
    }
    false
}

fn relative_path_has_noise_within_root(path: &Path, root: &Path) -> bool {
    path.strip_prefix(root)
        .ok()
        .is_some_and(|relative| path_has_noise_component(relative))
}

fn is_site_structure_event(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_))
    )
}

/// Whether a filesystem event path should trigger a content rebuild.
pub fn should_trigger_rebuild(
    path: &Path,
    kind: &EventKind,
    site_roots: &[PathBuf],
    theme_dirs: &[PathBuf],
) -> bool {
    if theme_dirs.iter().any(|dir| {
        path.starts_with(dir)
            && !relative_path_has_noise_within_root(path, dir)
            && is_theme_watch_path(path)
    }) {
        return true;
    }
    let Some(site_root) = site_roots.iter().find(|root| path.starts_with(root)) else {
        return false;
    };
    if relative_path_has_noise_within_root(path, site_root) {
        return false;
    }
    if is_markdown_path(path) {
        return true;
    }
    is_site_structure_event(kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_build_artifact_paths() {
        let site_root = PathBuf::from("/tmp/site");
        let path = PathBuf::from("/tmp/site/target/debug/readme.md");
        assert!(!should_trigger_rebuild(
            &path,
            &EventKind::Modify(ModifyKind::Data(notify::event::DataChange::Any)),
            &[site_root],
            &[]
        ));
    }

    #[test]
    fn accepts_markdown_in_site_root() {
        let site_root = PathBuf::from("/tmp/site");
        let path = PathBuf::from("/tmp/site/guide/readme.md");
        assert!(should_trigger_rebuild(
            &path,
            &EventKind::Modify(ModifyKind::Data(notify::event::DataChange::Any)),
            &[site_root],
            &[]
        ));
    }

    #[test]
    fn accepts_new_subfolder_creation() {
        let site_root = PathBuf::from("/tmp/site");
        let path = PathBuf::from("/tmp/site/guide");
        assert!(should_trigger_rebuild(
            &path,
            &EventKind::Create(notify::event::CreateKind::Folder),
            &[site_root],
            &[]
        ));
    }

    #[test]
    fn accepts_subfolder_removal() {
        let site_root = PathBuf::from("/tmp/site");
        let path = PathBuf::from("/tmp/site/guide");
        assert!(should_trigger_rebuild(
            &path,
            &EventKind::Remove(notify::event::RemoveKind::Folder),
            &[site_root],
            &[]
        ));
    }

    #[test]
    fn ignores_non_markdown_site_file_modifications() {
        let site_root = PathBuf::from("/tmp/site");
        let path = PathBuf::from("/tmp/site/src/main.rs");
        assert!(!should_trigger_rebuild(
            &path,
            &EventKind::Modify(ModifyKind::Data(notify::event::DataChange::Any)),
            &[site_root],
            &[]
        ));
    }

    #[test]
    fn accepts_non_markdown_site_file_creation() {
        let site_root = PathBuf::from("/tmp/site");
        let path = PathBuf::from("/tmp/site/logo.png");
        assert!(should_trigger_rebuild(
            &path,
            &EventKind::Create(notify::event::CreateKind::File),
            &[site_root],
            &[]
        ));
    }

    #[test]
    fn accepts_theme_template_changes() {
        let theme_dir = PathBuf::from("/tmp/theme");
        let path = PathBuf::from("/tmp/theme/layouts/doc.html");
        assert!(should_trigger_rebuild(
            &path,
            &EventKind::Modify(ModifyKind::Data(notify::event::DataChange::Any)),
            &[],
            &[theme_dir]
        ));
    }

    #[test]
    fn accepts_markdown_under_dot_parent_directory() {
        let site_root = PathBuf::from("/Users/patryk/.config/mdshelf/content");
        let path = PathBuf::from("/Users/patryk/.config/mdshelf/content/guide/readme.md");
        assert!(should_trigger_rebuild(
            &path,
            &EventKind::Create(notify::event::CreateKind::File),
            &[site_root],
            &[]
        ));
    }

    #[test]
    fn accepts_new_markdown_file_creation() {
        let site_root = PathBuf::from("/tmp/site");
        let path = PathBuf::from("/tmp/site/new-page.md");
        assert!(should_trigger_rebuild(
            &path,
            &EventKind::Create(notify::event::CreateKind::File),
            &[site_root],
            &[]
        ));
    }
}
