//! Orchestration: clone, collect, render, and hand the result somewhere.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use tracing::{info, warn};

use crate::clone::Cloner;
use crate::collect;
use crate::config::Config;
use crate::render::{self, Section};

/// Environment variable holding the PAT used for repositories marked private.
pub const TOKEN_VAR: &str = "NOTES_REPO_TOKEN";

#[derive(Debug, Clone)]
pub struct Options {
    /// Render to disk and skip Google Drive entirely.
    pub dry_run: bool,
    /// Where rendered Markdown is written. Written on every run, not just dry ones,
    /// so a scheduled run leaves an artifact that can be diffed without Drive access.
    pub out: PathBuf,
    /// Restrict the run to these repositories, by config name.
    pub only: Vec<String>,
}

/// A finished document, named as it will appear in Drive.
#[derive(Debug, Clone)]
pub struct Document {
    pub name: String,
    pub markdown: String,
}

/// Clones every selected repository and renders the configured documents.
pub fn build(config: &Config, options: &Options) -> Result<Vec<Document>> {
    let repos = select_repos(config, &options.only)?;

    let token = std::env::var(TOKEN_VAR).ok();
    if let Some(token) = &token {
        crate::clone::mask_in_actions(token);
    }
    let cloner = Cloner::new(token);

    let generated_at = now_utc();
    let mut sections = Vec::with_capacity(repos.len());

    for repo in repos {
        let checkout = cloner
            .checkout(repo)
            .with_context(|| format!("checking out {}", repo.name))?;

        let files = collect::collect(&checkout.walk_root, repo)
            .with_context(|| format!("collecting Markdown from {}", repo.name))?;

        if files.is_empty() {
            warn!(
                repo = %repo.name,
                "no Markdown matched; check the include and exclude globs"
            );
        }
        info!(repo = %repo.name, files = files.len(), "collected");

        sections.push(Section {
            name: repo.name.clone(),
            url: repo.url.clone(),
            commit: checkout.commit.clone(),
            files,
            strip_frontmatter: repo.strip_frontmatter,
        });
    }

    let mut documents = Vec::new();

    if config.drive.emit_per_repo {
        for section in &sections {
            documents.push(Document {
                name: section.name.clone(),
                markdown: render::render(
                    &section.name,
                    std::slice::from_ref(section),
                    &generated_at,
                ),
            });
        }
    }

    if config.drive.emit_combined {
        documents.push(Document {
            name: config.drive.combined_doc_name.clone(),
            markdown: render::render(&config.drive.combined_doc_name, &sections, &generated_at),
        });
    }

    Ok(documents)
}

/// Writes each document to `dir` as Markdown, returning the paths written.
pub fn write_local(documents: &[Document], dir: &Path) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("creating output directory {}", dir.display()))?;

    let mut written = Vec::with_capacity(documents.len());
    for document in documents {
        let path = dir.join(format!("{}.md", safe_file_stem(&document.name)));
        std::fs::write(&path, &document.markdown)
            .with_context(|| format!("writing {}", path.display()))?;
        written.push(path);
    }
    Ok(written)
}

fn select_repos<'a>(config: &'a Config, only: &[String]) -> Result<Vec<&'a crate::config::Repo>> {
    if only.is_empty() {
        return Ok(config.repos.iter().collect());
    }

    let known: HashSet<&str> = config.repos.iter().map(|r| r.name.as_str()).collect();
    // A typo in --only should not quietly sync nothing.
    for name in only {
        if !known.contains(name.as_str()) {
            bail!(
                "--only {name:?} does not match any repository in the config; known names: {}",
                config
                    .repos
                    .iter()
                    .map(|r| format!("{:?}", r.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }

    Ok(config
        .repos
        .iter()
        .filter(|r| only.iter().any(|n| n == &r.name))
        .collect())
}

/// Makes a document name safe to use as a file name on disk.
fn safe_file_stem(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').trim();
    if trimmed.is_empty() {
        "document".to_string()
    } else {
        trimmed.to_string()
    }
}

fn now_utc() -> String {
    jiff::Timestamp::now()
        .strftime("%Y-%m-%d %H:%M UTC")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Repo;

    fn config_with(names: &[&str]) -> Config {
        let yaml = format!(
            "drive:\n  folder_id: \"f\"\nrepos:\n{}",
            names
                .iter()
                .map(|n| format!("  - url: https://example.invalid/{n}\n    name: \"{n}\"\n"))
                .collect::<String>()
        );
        Config::from_yaml(&yaml, Path::new("repos.yml")).unwrap()
    }

    fn names(repos: &[&Repo]) -> Vec<String> {
        repos.iter().map(|r| r.name.clone()).collect()
    }

    #[test]
    fn selects_every_repo_when_only_is_empty() {
        let config = config_with(&["a", "b"]);
        assert_eq!(names(&select_repos(&config, &[]).unwrap()), ["a", "b"]);
    }

    #[test]
    fn only_narrows_the_selection() {
        let config = config_with(&["a", "b", "c"]);
        let selected = select_repos(&config, &["c".into(), "a".into()]).unwrap();
        // Config order is kept, not the order the flags were given in.
        assert_eq!(names(&selected), ["a", "c"]);
    }

    #[test]
    fn an_unknown_only_name_is_an_error_rather_than_an_empty_run() {
        let config = config_with(&["a", "b"]);
        let err = select_repos(&config, &["typo".into()])
            .unwrap_err()
            .to_string();
        assert!(err.contains("does not match any repository"), "{err}");
        assert!(err.contains("\"a\", \"b\""), "{err}");
    }

    #[test]
    fn document_names_become_safe_file_stems() {
        assert_eq!(safe_file_stem("Notes — All Repos"), "Notes - All Repos");
        assert_eq!(safe_file_stem("a/b:c"), "a-b-c");
        assert_eq!(safe_file_stem("  ..  "), "document");
    }

    #[test]
    fn writes_one_file_per_document() {
        let dir = tempfile::tempdir().unwrap();
        let documents = vec![
            Document {
                name: "Personal Notes".into(),
                markdown: "# Personal Notes\n".into(),
            },
            Document {
                name: "Notes — All".into(),
                markdown: "# All\n".into(),
            },
        ];

        let written = write_local(&documents, dir.path()).unwrap();

        assert_eq!(written.len(), 2);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("Personal Notes.md")).unwrap(),
            "# Personal Notes\n"
        );
        assert!(dir.path().join("Notes - All.md").is_file());
    }
}
