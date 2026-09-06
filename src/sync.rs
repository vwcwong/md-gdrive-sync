//! Orchestration: clone, collect, render, and hand the result somewhere.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use tracing::{info, warn};

use crate::clone::{Cloner, mask_in_actions};
use crate::collect;
use crate::collect::MarkdownFile;
use crate::config::Config;
use crate::drive::auth::{self, OauthClient};
use crate::drive::files::{DriveClient, Upsert};
use crate::render::{self, Section};

pub use crate::clone::TOKEN_VAR;

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
        mask_in_actions(token);
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

    let max_chars = config.drive.max_chars_per_doc;
    let mut documents = Vec::new();

    if config.drive.emit_per_repo {
        for section in &sections {
            documents.extend(documents_for(
                &section.name,
                std::slice::from_ref(section),
                &generated_at,
                max_chars,
            ));
        }
    }

    if config.drive.emit_combined {
        documents.extend(documents_for(
            &config.drive.combined_doc_name,
            &sections,
            &generated_at,
            max_chars,
        ));
    }

    Ok(documents)
}

/// Publishes each document to the configured Drive folder, updating existing
/// documents in place rather than replacing them.
pub fn publish(config: &Config, documents: &[Document], options: &Options) -> Result<()> {
    let oauth = OauthClient::from_env().context(
        "Google credentials are missing; run `mdsync auth` and set GOOGLE_CLIENT_ID, \
         GOOGLE_CLIENT_SECRET and GOOGLE_REFRESH_TOKEN",
    )?;

    let refresh_token = auth::required_env(auth::REFRESH_TOKEN_VAR)?;
    mask_in_actions(&refresh_token);

    let access_token = oauth.access_token(&refresh_token)?;
    mask_in_actions(&access_token);
    let drive = DriveClient::new(access_token)?;

    for document in documents {
        let outcome = drive
            .upsert_doc(&config.drive.folder_id, &document.name, &document.markdown)
            .with_context(|| format!("publishing {:?}", document.name))?;

        match outcome {
            Upsert::Created => println!(
                "created {:?} — add it to NotebookLM once; later runs update it in place",
                document.name
            ),
            Upsert::Updated => println!("updated {:?}", document.name),
        }
    }

    if config.drive.prune_orphans {
        // Skipped for a partial run: --only deliberately syncs a subset, and
        // everything else in the folder would look like an orphan.
        if options.only.is_empty() {
            prune(&drive, &config.drive.folder_id, documents)?;
        } else {
            info!("skipping prune: --only was given, so the folder is not fully represented");
        }
    }

    Ok(())
}

/// Trashes documents this tool created in the folder that no longer correspond
/// to anything in the config. Trashed rather than deleted, so a mistake is
/// recoverable from the Drive bin.
fn prune(drive: &DriveClient, folder_id: &str, documents: &[Document]) -> Result<()> {
    let keep: HashSet<&str> = documents.iter().map(|d| d.name.as_str()).collect();

    for existing in drive.list_folder(folder_id)? {
        if keep.contains(existing.name.as_str()) {
            continue;
        }
        drive
            .trash(&existing.id)
            .with_context(|| format!("trashing orphaned document {:?}", existing.name))?;
        println!("trashed orphaned document {:?}", existing.name);
    }

    Ok(())
}

/// Renders `sections` into as many documents as the size limit requires.
///
/// Drive truncates an oversized document silently and NotebookLM caps a source
/// at 500k words, so it is split instead — on file boundaries, since half a note
/// is worse than a second document.
fn documents_for(
    name: &str,
    sections: &[Section],
    generated_at: &str,
    max_chars: usize,
) -> Vec<Document> {
    let groups = split_sections(sections, max_chars);

    groups
        .into_iter()
        .enumerate()
        .map(|(index, group)| {
            let name = part_name(name, index);
            let markdown = render::render(&name, &group, generated_at);

            if markdown.chars().count() > max_chars {
                warn!(
                    document = %name,
                    chars = markdown.chars().count(),
                    limit = max_chars,
                    "document exceeds the size limit even on its own; a single file must be \
                     larger than the limit"
                );
            }

            Document { name, markdown }
        })
        .collect()
}

/// The first part keeps the bare name, so a document that grows and starts
/// splitting does not orphan the NotebookLM source already pointing at it.
fn part_name(name: &str, index: usize) -> String {
    if index == 0 {
        name.to_string()
    } else {
        format!("{name} (Part {})", index + 1)
    }
}

/// Packs files into groups that should each render within `max_chars`.
///
/// Sizes are estimated from the source; measuring by rendering candidates would
/// be quadratic. The rendered result is checked afterwards.
fn split_sections(sections: &[Section], max_chars: usize) -> Vec<Vec<Section>> {
    let overhead = DOCUMENT_OVERHEAD + PER_SECTION_OVERHEAD * sections.len();
    let budget = max_chars.saturating_sub(overhead).max(1);

    let mut groups: Vec<Vec<Section>> = Vec::new();
    let mut current: Vec<Section> = Vec::new();
    let mut used = 0usize;

    for section in sections {
        // A repository that matched nothing still earns a provenance line, so
        // an empty result is visible rather than looking like a missing repo.
        if section.files.is_empty() {
            current.push(section_with(section, Vec::new()));
            continue;
        }

        let mut pending: Vec<MarkdownFile> = Vec::new();

        for file in &section.files {
            let cost = estimated_cost(file);

            let would_overflow = used + cost > budget;
            let has_content = !pending.is_empty() || !current.is_empty();
            if would_overflow && has_content {
                if !pending.is_empty() {
                    current.push(section_with(section, std::mem::take(&mut pending)));
                }
                groups.push(std::mem::take(&mut current));
                used = 0;
            }

            pending.push(file.clone());
            used += cost;
        }

        if !pending.is_empty() {
            current.push(section_with(section, pending));
        }
    }

    if !current.is_empty() || groups.is_empty() {
        groups.push(current);
    }
    groups
}

/// Rendering adds a path heading and blank lines around each file.
fn estimated_cost(file: &MarkdownFile) -> usize {
    file.content.chars().count() + file.rel_path.chars().count() + PER_FILE_OVERHEAD
}

fn section_with(section: &Section, files: Vec<MarkdownFile>) -> Section {
    Section {
        files,
        ..section.clone()
    }
}

/// Title and generation note.
const DOCUMENT_OVERHEAD: usize = 400;
/// A provenance line per repository.
const PER_SECTION_OVERHEAD: usize = 200;
/// Path heading, blank lines, and room for heading demotion.
const PER_FILE_OVERHEAD: usize = 32;

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
            if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.' | '(' | ')') {
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
        // Part suffixes survive, so the local artifact matches the Drive name.
        assert_eq!(safe_file_stem("Notes (Part 2)"), "Notes (Part 2)");
        assert_eq!(safe_file_stem("  ..  "), "document");
    }

    fn file(rel_path: &str, chars: usize) -> MarkdownFile {
        MarkdownFile {
            rel_path: rel_path.to_string(),
            content: "x".repeat(chars),
        }
    }

    fn section(name: &str, files: Vec<MarkdownFile>) -> Section {
        Section {
            name: name.to_string(),
            url: "https://example.invalid/x".into(),
            commit: "abc1234def".into(),
            files,
            strip_frontmatter: true,
        }
    }

    #[test]
    fn a_document_within_the_limit_is_not_split() {
        let sections = vec![section("R", vec![file("a.md", 100), file("b.md", 100)])];
        let documents = documents_for("Notes", &sections, "t", 900_000);

        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].name, "Notes");
    }

    #[test]
    fn an_oversized_document_splits_into_named_parts() {
        let sections = vec![section(
            "R",
            vec![file("a.md", 800), file("b.md", 800), file("c.md", 800)],
        )];
        // Room for roughly one file per part once overhead is taken off.
        let documents = documents_for("Notes", &sections, "t", 1_500);

        assert!(documents.len() >= 3, "{:?}", documents.len());
        assert_eq!(documents[0].name, "Notes");
        assert_eq!(documents[1].name, "Notes (Part 2)");
        assert_eq!(documents[2].name, "Notes (Part 3)");
    }

    #[test]
    fn splitting_never_divides_a_file() {
        let sections = vec![section(
            "R",
            vec![file("a.md", 800), file("b.md", 800), file("c.md", 800)],
        )];
        let documents = documents_for("Notes", &sections, "t", 1_500);

        // Every file's heading appears exactly once across all the parts.
        for path in ["a.md", "b.md", "c.md"] {
            let hits: usize = documents
                .iter()
                .map(|d| d.markdown.matches(&format!("\n## {path}\n")).count())
                .sum();
            assert_eq!(hits, 1, "{path} appeared {hits} times");
        }
    }

    #[test]
    fn a_single_file_larger_than_the_limit_still_gets_a_document() {
        let sections = vec![section("R", vec![file("huge.md", 5_000)])];
        let documents = documents_for("Notes", &sections, "t", 1_000);

        assert_eq!(documents.len(), 1);
        assert!(documents[0].markdown.contains("huge.md"));
    }

    #[test]
    fn a_repo_that_matched_nothing_still_appears_in_the_document() {
        let sections = vec![section("Empty", Vec::new())];
        let documents = documents_for("Notes", &sections, "t", 900_000);

        assert_eq!(documents.len(), 1);
        assert!(
            documents[0].markdown.contains("**Empty**"),
            "{}",
            documents[0].markdown
        );
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
