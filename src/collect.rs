//! Walking a checked-out repository for the Markdown files named by its config.

use std::path::Path;

use anyhow::{Context, Result};
use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;
use tracing::{debug, warn};

use crate::config::Repo;

/// One source file, ready to render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownFile {
    /// Path relative to the walk root, always with `/` separators so the
    /// rendered document reads the same on any platform.
    pub rel_path: String,
    pub content: String,
}

/// Collects the Markdown under `root` that the repo's include/exclude globs select.
///
/// Files ignored by the repository's own `.gitignore`, and dotfiles, are skipped:
/// if a note is not worth committing it is not worth syncing, and directories
/// like `.obsidian/` are editor state rather than notes.
pub fn collect(root: &Path, repo: &Repo) -> Result<Vec<MarkdownFile>> {
    let mut overrides = OverrideBuilder::new(root);

    // In this crate a plain glob whitelists and a leading `!` ignores, which is
    // the inverse of gitignore. Excludes are added last so they win.
    for pattern in &repo.include {
        overrides
            .add(pattern)
            .with_context(|| format!("{}: bad include glob {pattern:?}", repo.name))?;
    }
    for pattern in &repo.exclude {
        overrides
            .add(&format!("!{pattern}"))
            .with_context(|| format!("{}: bad exclude glob {pattern:?}", repo.name))?;
    }
    let overrides = overrides
        .build()
        .with_context(|| format!("{}: could not build the file filter", repo.name))?;

    let mut files = Vec::new();

    for entry in WalkBuilder::new(root).overrides(overrides).build() {
        let entry = entry.with_context(|| format!("{}: walking {}", repo.name, root.display()))?;

        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }

        let rel_path = relative_slash_path(root, entry.path());

        // Notes are text; a file that is not UTF-8 is not something NotebookLM
        // could read anyway, so warn and carry on rather than failing the run.
        let content = match std::fs::read_to_string(entry.path()) {
            Ok(content) => content,
            Err(err) if err.kind() == std::io::ErrorKind::InvalidData => {
                warn!(repo = %repo.name, file = %rel_path, "skipping: not valid UTF-8");
                continue;
            }
            Err(err) => {
                return Err(err).with_context(|| format!("{}: reading {rel_path}", repo.name));
            }
        };

        if content.trim().is_empty() {
            debug!(repo = %repo.name, file = %rel_path, "skipping: empty");
            continue;
        }

        files.push(MarkdownFile { rel_path, content });
    }

    files.sort_by(|a, b| sort_key(&a.rel_path).cmp(&sort_key(&b.rel_path)));

    debug!(repo = %repo.name, count = files.len(), "collected");
    Ok(files)
}

/// Orders paths the way the rendered document should read: within a directory,
/// its own files first, then its subdirectories, each group alphabetical.
///
/// Putting a directory's files ahead of its subdirectories means a folder's own
/// notes appear under its heading before the headings for anything nested
/// inside it, the way an introduction precedes its subsections.
///
/// The ordering is total and content-independent, so an unchanged set of files
/// renders byte-for-byte identically on every run.
fn sort_key(rel_path: &str) -> Vec<(u8, &str)> {
    let count = rel_path.split('/').count();
    rel_path
        .split('/')
        .enumerate()
        // 0 sorts the final component (the file name) ahead of 1, the
        // directory components at the same level.
        .map(|(i, part)| (if i + 1 == count { 0 } else { 1 }, part))
        .collect()
}

fn relative_slash_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn repo(include: &[&str], exclude: &[&str]) -> Repo {
        Repo {
            url: "https://example.invalid/x".into(),
            name: "fixture".into(),
            private: false,
            branch: None,
            subdir: None,
            include: include.iter().map(|s| s.to_string()).collect(),
            exclude: exclude.iter().map(|s| s.to_string()).collect(),
            strip_frontmatter: true,
        }
    }

    fn tree(files: &[(&str, &str)]) -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, content) in files {
            let full = dir.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, content).unwrap();
        }
        dir
    }

    fn paths(files: &[MarkdownFile]) -> Vec<&str> {
        files.iter().map(|f| f.rel_path.as_str()).collect()
    }

    #[test]
    fn collects_markdown_and_ignores_other_extensions() {
        let dir = tree(&[
            ("README.md", "# Readme"),
            ("notes.markdown", "# Notes"),
            ("image.png", "not markdown"),
            ("script.sh", "echo hi"),
        ]);

        let files = collect(dir.path(), &repo(&["**/*.md", "**/*.markdown"], &[])).unwrap();
        assert_eq!(paths(&files), ["README.md", "notes.markdown"]);
    }

    #[test]
    fn applies_exclude_globs_over_includes() {
        let dir = tree(&[
            ("keep.md", "# Keep"),
            ("drafts/skip.md", "# Skip"),
            ("CHANGELOG.md", "# Changelog"),
        ]);

        let files = collect(
            dir.path(),
            &repo(&["**/*.md"], &["drafts/**", "**/CHANGELOG.md"]),
        )
        .unwrap();
        assert_eq!(paths(&files), ["keep.md"]);
    }

    #[test]
    fn orders_a_directorys_own_files_before_its_subdirectories() {
        let dir = tree(&[
            ("docs/api/auth.md", "x"),
            ("docs/zebra.md", "x"),
            ("docs/alpha.md", "x"),
            ("README.md", "x"),
            ("archive/old.md", "x"),
        ]);

        let files = collect(dir.path(), &repo(&["**/*.md"], &[])).unwrap();
        assert_eq!(
            paths(&files),
            [
                "README.md",
                "archive/old.md",
                "docs/alpha.md",
                "docs/zebra.md",
                "docs/api/auth.md",
            ]
        );
    }

    #[test]
    fn ordering_is_stable_across_runs() {
        let dir = tree(&[
            ("b.md", "x"),
            ("a/c.md", "x"),
            ("a/b/d.md", "x"),
            ("a.md", "x"),
        ]);

        let first = collect(dir.path(), &repo(&["**/*.md"], &[])).unwrap();
        let second = collect(dir.path(), &repo(&["**/*.md"], &[])).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn skips_empty_files() {
        let dir = tree(&[("real.md", "# Real"), ("blank.md", "   \n\n  ")]);

        let files = collect(dir.path(), &repo(&["**/*.md"], &[])).unwrap();
        assert_eq!(paths(&files), ["real.md"]);
    }

    #[test]
    fn skips_dotfiles_and_editor_state() {
        let dir = tree(&[(".obsidian/workspace.md", "x"), ("real.md", "# Real")]);

        let files = collect(dir.path(), &repo(&["**/*.md"], &[])).unwrap();
        assert_eq!(paths(&files), ["real.md"]);
    }

    #[test]
    fn respects_the_repositorys_gitignore() {
        let dir = tree(&[
            ("real.md", "# Real"),
            ("build/generated.md", "# Generated"),
            (".gitignore", "build/\n"),
        ]);
        // .gitignore is only honoured inside a git repository, which is always
        // the case for a real checkout.
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();

        let files = collect(dir.path(), &repo(&["**/*.md"], &[])).unwrap();
        assert_eq!(paths(&files), ["real.md"]);
    }

    #[test]
    fn narrower_include_selects_a_single_subtree() {
        let dir = tree(&[("docs/a.md", "x"), ("other/b.md", "x"), ("c.md", "x")]);

        let files = collect(dir.path(), &repo(&["docs/**/*.md"], &[])).unwrap();
        assert_eq!(paths(&files), ["docs/a.md"]);
    }

    #[test]
    fn reads_file_contents() {
        let dir = tree(&[("a.md", "# Title\n\nbody\n")]);

        let files = collect(dir.path(), &repo(&["**/*.md"], &[])).unwrap();
        assert_eq!(files[0].content, "# Title\n\nbody\n");
    }

    #[test]
    fn skips_files_that_are_not_utf8() {
        let dir = tree(&[("good.md", "# Good")]);
        std::fs::write(dir.path().join("bad.md"), [0xff, 0xfe, 0x00]).unwrap();

        let files = collect(dir.path(), &repo(&["**/*.md"], &[])).unwrap();
        assert_eq!(paths(&files), ["good.md"]);
    }
}
