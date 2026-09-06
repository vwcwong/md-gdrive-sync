//! Splitting rendered output into Drive-sized documents on file boundaries.

use tracing::warn;

use crate::collect::MarkdownFile;
use crate::render::{self, Section};

/// A finished document, named as it will appear in Drive.
#[derive(Debug, Clone)]
pub struct Document {
    pub name: String,
    pub markdown: String,
}

/// Renders `sections` into as many documents as the size limit requires.
///
/// Drive truncates an oversized document silently and NotebookLM caps a source
/// at 500k words, so it is split instead — on file boundaries, since half a note
/// is worse than a second document.
pub fn documents_for(
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

#[cfg(test)]
mod tests {
    use super::*;

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

    fn oversized_section() -> Vec<Section> {
        vec![section(
            "R",
            vec![file("a.md", 800), file("b.md", 800), file("c.md", 800)],
        )]
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
        // Room for roughly one file per part once overhead is taken off.
        let documents = documents_for("Notes", &oversized_section(), "t", 1_500);

        assert!(documents.len() >= 3, "{:?}", documents.len());
        assert_eq!(documents[0].name, "Notes");
        assert_eq!(documents[1].name, "Notes (Part 2)");
        assert_eq!(documents[2].name, "Notes (Part 3)");
    }

    #[test]
    fn splitting_never_divides_a_file() {
        let documents = documents_for("Notes", &oversized_section(), "t", 1_500);

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
}
