//! Turning collected Markdown into a single document whose heading outline
//! mirrors the original directory structure.
//!
//! Everything here is pure text in, text out: no filesystem, no network, no
//! clock. The generation timestamp is passed in so output is reproducible and
//! snapshot-testable.

use std::borrow::Cow;
use std::ops::Range;

use pulldown_cmark::{Event, Options, Parser, Tag};
use serde::Deserialize;

use crate::collect::MarkdownFile;

/// Markdown, and Google Docs, stop at six heading levels.
const MAX_HEADING: usize = 6;

/// One repository's contribution to a document.
#[derive(Debug, Clone)]
pub struct Section {
    pub name: String,
    pub url: String,
    pub commit: String,
    pub files: Vec<MarkdownFile>,
    pub strip_frontmatter: bool,
}

/// Renders `sections` into one document titled `title`.
///
/// A single-repository document omits the per-repository heading, which the
/// title already names, freeing a level for deep directory trees.
pub fn render(title: &str, sections: &[Section], generated_at: &str) -> String {
    let mut out = String::new();

    out.push_str(&format!("# {title}\n\n"));
    push_provenance(&mut out, sections, generated_at);

    let single = sections.len() == 1;
    for section in sections {
        let base = if single {
            1
        } else {
            out.push_str(&format!("## {}\n\n", section.name));
            2
        };
        push_section(&mut out, section, base);
    }

    // One trailing newline, however the last file happened to end.
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}

fn push_provenance(out: &mut String, sections: &[Section], generated_at: &str) {
    out.push_str(&format!(
        "*Generated {generated_at} by mdsync. Do not edit: this document is overwritten on \
         every sync.*\n\n"
    ));

    out.push_str("**Sources**\n\n");
    for section in sections {
        out.push_str(&format!(
            "- **{}** — `{}` at commit `{}` ({} {})\n",
            section.name,
            section.url,
            short_commit(&section.commit),
            section.files.len(),
            plural(section.files.len(), "file", "files"),
        ));
    }
    out.push('\n');
}

/// Emits one heading per file carrying its full path, rather than a heading per
/// directory level.
///
/// Six levels cannot cover a title, a repository, a directory tree and the
/// file's own headings, so the tree gives up its levels: a path in a heading
/// conveys the structure losslessly, whereas squashed content headings lose it
/// for good.
fn push_section(out: &mut String, section: &Section, base: usize) {
    let level = base + 1;

    for file in &section.files {
        push_heading(out, level, &file.rel_path);

        let (frontmatter_title, body) = transform(&file.content, level, section.strip_frontmatter);

        // A frontmatter title stands in for the H1 the file does not have, so
        // it sits at the level the body's own H1 would occupy.
        if let Some(title) = &frontmatter_title {
            push_heading(out, level + 1, title);
        }

        out.push_str(body.trim_start_matches(['\n', '\r']).trim_end());
        out.push_str("\n\n");
    }
}

fn push_heading(out: &mut String, level: usize, text: &str) {
    out.push_str(&"#".repeat(level.min(MAX_HEADING)));
    out.push(' ');
    out.push_str(text);
    out.push_str("\n\n");
}

/// Demotes headings, replaces images with a placeholder, and lifts the title out
/// of any frontmatter.
///
/// Only those three byte ranges are rewritten; everything else is copied
/// verbatim, so nothing else can be reformatted on the way through.
///
/// Levels are normalised, not shifted: a file starting at H2 lands directly
/// under the path heading rather than leaving a gap.
fn transform(content: &str, offset: usize, strip_frontmatter: bool) -> (Option<String>, String) {
    // Everything written around the body uses `\n`, so a CRLF file would leave
    // the document with two kinds of line ending in it.
    let content = if content.contains("\r\n") {
        Cow::Owned(content.replace("\r\n", "\n"))
    } else {
        Cow::Borrowed(content)
    };
    let content: &str = &content;

    let mut options = Options::ENABLE_TABLES;
    if strip_frontmatter {
        options |= Options::ENABLE_YAML_STYLE_METADATA_BLOCKS;
    }

    let top = Parser::new_ext(content, options)
        .filter_map(|event| match event {
            Event::Start(Tag::Heading { level, .. }) => Some(level as usize),
            _ => None,
        })
        .min()
        .unwrap_or(1);

    let mut title = None;
    let mut out = String::with_capacity(content.len());
    let mut cursor = 0;
    let mut events = Parser::new_ext(content, options).into_offset_iter();

    while let Some((event, range)) = events.next() {
        let (range, replacement) = match event {
            Event::Start(Tag::Heading { level, .. }) => {
                let text = inner_range(&mut events)
                    .map(|inner| content[inner].trim())
                    .unwrap_or_default();
                let raw = &content[range.clone()];
                let newline = &raw[raw.trim_end_matches(['\r', '\n']).len()..];
                let hashes = "#".repeat((level as usize + offset + 1 - top).min(MAX_HEADING));
                let rewritten = if text.is_empty() {
                    format!("{hashes}{newline}")
                } else {
                    format!("{hashes} {text}{newline}")
                };
                (range, rewritten)
            }
            Event::Start(Tag::Image { .. }) => (range, image_placeholder(&inner_text(&mut events))),
            Event::Start(Tag::MetadataBlock(_)) => {
                title = frontmatter_title(&inner_text(&mut events));
                // Take the blank line the block leaves behind with it.
                let mut end = range.end;
                end += content[end..].len() - content[end..].trim_start_matches(['\r', '\n']).len();
                (range.start..end, String::new())
            }
            _ => continue,
        };

        out.push_str(&content[cursor..range.start]);
        out.push_str(&replacement);
        cursor = range.end;
    }

    out.push_str(&content[cursor..]);
    (title, out)
}

/// Consumes the events closing the tag just started, returning the source they
/// span. Nesting is counted, so emphasis inside a heading does not end it early.
fn inner_range<'a>(
    events: &mut impl Iterator<Item = (Event<'a>, Range<usize>)>,
) -> Option<Range<usize>> {
    let mut span: Option<Range<usize>> = None;
    let mut depth = 1usize;

    for (event, range) in events {
        match event {
            Event::Start(_) => depth += 1,
            Event::End(_) => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        span = Some(match span {
            Some(span) => span.start..range.end,
            None => range,
        });
    }

    span
}

/// The same, for tags whose text is wanted rather than their source.
fn inner_text<'a>(events: &mut impl Iterator<Item = (Event<'a>, Range<usize>)>) -> String {
    let mut text = String::new();
    let mut depth = 1usize;

    for (event, _) in events {
        match event {
            Event::Start(_) => depth += 1,
            Event::End(_) => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            Event::Text(run) | Event::Code(run) => text.push_str(&run),
            _ => {}
        }
    }

    text
}

/// Drive turns Markdown images into base64 data URIs that render broken in a
/// Google Doc. The alt text is what carries meaning for NotebookLM anyway, so
/// keep that and drop the reference.
fn image_placeholder(alt: &str) -> String {
    if alt.trim().is_empty() {
        "*[image]*".to_string()
    } else {
        format!("*[image: {alt}]*")
    }
}

#[derive(Deserialize)]
struct Frontmatter {
    title: Option<String>,
}

/// Frontmatter is written by hand and not validated anywhere, so anything that
/// does not parse simply has no title rather than failing the sync.
fn frontmatter_title(yaml: &str) -> Option<String> {
    let frontmatter: Frontmatter = serde_saphyr::from_str(yaml).ok()?;
    frontmatter
        .title
        .map(|title| title.trim().to_string())
        .filter(|title| !title.is_empty())
}

fn short_commit(commit: &str) -> &str {
    &commit[..commit.len().min(8)]
}

fn plural<'a>(n: usize, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 { one } else { many }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(rel_path: &str, content: &str) -> MarkdownFile {
        MarkdownFile {
            rel_path: rel_path.to_string(),
            content: content.to_string(),
        }
    }

    fn section(name: &str, files: Vec<MarkdownFile>) -> Section {
        Section {
            name: name.to_string(),
            url: format!("https://example.invalid/{name}"),
            commit: "0123456789abcdef0123456789abcdef01234567".into(),
            files,
            strip_frontmatter: true,
        }
    }

    /// The rewritten body of a file whose path heading sits at `level`.
    fn transformed(content: &str, level: usize) -> String {
        transform(content, level, true).1
    }

    #[test]
    fn demotes_headings_by_the_file_depth() {
        assert_eq!(transformed("# One\n## Two\n", 3), "#### One\n##### Two\n");
    }

    #[test]
    fn caps_demotion_at_six_levels() {
        // Six levels of the file's own headings cannot all survive under a
        // heading that is already at level 3.
        let out = transformed("# 1\n## 2\n### 3\n#### 4\n##### 5\n###### 6\n", 3);
        assert!(out.ends_with("###### 5\n###### 6\n"), "{out}");
    }

    #[test]
    fn normalises_a_file_whose_top_heading_is_not_h1() {
        // Starts at H2, so H2 lands directly under the path heading rather than
        // leaving a gap, and the relative nesting is kept.
        assert_eq!(
            transformed("## Top\n### Under\n", 2),
            "### Top\n#### Under\n"
        );
    }

    #[test]
    fn normalisation_uses_the_shallowest_heading_not_the_first() {
        assert_eq!(
            transformed("### Third\n## Second\n", 1),
            "### Third\n## Second\n"
        );
    }

    #[test]
    fn leaves_hashes_inside_fenced_code_alone() {
        let body = "text\n\n```sh\n# not a heading\necho hi\n```\n\n# real heading\n";
        let out = transformed(body, 2);
        assert!(out.contains("\n# not a heading\n"), "{out}");
        assert!(out.contains("\n### real heading\n"), "{out}");
    }

    #[test]
    fn rewrites_setext_headings_so_they_can_be_demoted() {
        let out = transformed("Title\n=====\n\nSub\n---\n", 2);
        assert!(out.contains("### Title\n"), "{out}");
        assert!(out.contains("#### Sub\n"), "{out}");
    }

    /// A changelog's `1.3.4` over `=====` is a heading, not an ordered list.
    #[test]
    fn demotes_a_setext_heading_whose_text_looks_like_a_list_marker() {
        assert_eq!(
            transformed("1.3.4\n=====\n\nnotes\n", 2),
            "### 1.3.4\n\nnotes\n"
        );
    }

    #[test]
    fn normalises_crlf_so_the_document_has_one_kind_of_line_ending() {
        assert_eq!(transformed("# One\r\n\r\ntext\r\n", 1), "## One\n\ntext\n");
    }

    #[test]
    fn keeps_inline_markup_inside_a_demoted_heading() {
        assert_eq!(
            transformed("# Some **bold** `code`\n", 1),
            "## Some **bold** `code`\n"
        );
    }

    /// The whole point of rewriting byte ranges rather than re-rendering a parse
    /// tree: anything the transformation has no opinion about survives exactly.
    #[test]
    fn passes_what_it_does_not_rewrite_through_verbatim() {
        let body = "| a | b |\n|---|---|\n| 1 | 2 |\n\n* one\n+ two\n\n> quoted\n\n    indented code\n\n---\n";
        assert_eq!(transformed(body, 2), body);
    }

    #[test]
    fn replaces_images_with_their_alt_text() {
        assert_eq!(
            transformed("before ![a diagram](./x.png) after\n", 1),
            "before *[image: a diagram]* after\n"
        );
        assert_eq!(transformed("![](x.png)\n", 1), "*[image]*\n");
    }

    #[test]
    fn replaces_reference_style_images_too() {
        let out = transformed("![alt text][logo]\n\n[logo]: ./logo.png\n", 1);
        assert!(out.starts_with("*[image: alt text]*"), "{out}");
    }

    #[test]
    fn leaves_ordinary_links_alone() {
        let line = "see [the docs](https://example.invalid)\n";
        assert_eq!(transformed(line, 1), line);
    }

    #[test]
    fn does_not_replace_images_inside_code() {
        let out = transformed("```\n![keep](x.png)\n```\n", 1);
        assert!(out.contains("![keep](x.png)"), "{out}");
    }

    #[test]
    fn strips_frontmatter_and_takes_the_title_from_it() {
        let (title, body) = transform("---\ntitle: My Note\ntags: [a]\n---\n# Body\n", 1, true);
        assert_eq!(title.as_deref(), Some("My Note"));
        assert_eq!(body, "## Body\n");
    }

    #[test]
    fn unquotes_a_frontmatter_title() {
        let (title, _) = transform("---\ntitle: \"Quoted\"\n---\nbody\n", 1, true);
        assert_eq!(title.as_deref(), Some("Quoted"));
    }

    #[test]
    fn leaves_a_leading_horizontal_rule_alone() {
        let content = "---\n\nnot frontmatter, no closing delimiter\n";
        let (title, body) = transform(content, 1, true);
        assert_eq!(title, None);
        assert_eq!(body, content);
    }

    #[test]
    fn frontmatter_without_a_title_still_gets_stripped() {
        let (title, body) = transform("---\ntags: [a]\n---\nbody\n", 1, true);
        assert_eq!(title, None);
        assert_eq!(body, "body\n");
    }

    #[test]
    fn malformed_frontmatter_yields_no_title_rather_than_failing() {
        let (title, _) = transform("---\ntitle: [unclosed\n---\nbody\n", 1, true);
        assert_eq!(title, None);
    }

    #[test]
    fn single_repo_document_omits_the_repo_heading() {
        let out = render(
            "Personal Notes",
            &[section("Personal Notes", vec![file("a.md", "# A")])],
            "2026-01-01 00:00 UTC",
        );
        assert_eq!(out.matches("# Personal Notes").count(), 1, "{out}");
        assert!(out.contains("\n## a.md\n"), "{out}");
        assert!(out.contains("\n### A\n"), "{out}");
    }

    #[test]
    fn combined_document_nests_repos_one_level_deeper() {
        let out = render(
            "All",
            &[
                section("One", vec![file("a.md", "# A")]),
                section("Two", vec![file("docs/b.md", "# B")]),
            ],
            "2026-01-01 00:00 UTC",
        );
        assert!(out.contains("\n## One\n"), "{out}");
        assert!(out.contains("\n### a.md\n"), "{out}");
        assert!(out.contains("\n### docs/b.md\n"), "{out}");
    }

    #[test]
    fn every_file_heading_is_at_the_same_level_whatever_its_depth() {
        let out = render(
            "N",
            &[section(
                "N",
                vec![
                    file("a.md", "# H1"),
                    file("docs/b.md", "# H1"),
                    file("docs/api/deep/c.md", "# H1\n## H2\n### H3\n"),
                ],
            )],
            "t",
        );
        assert!(out.contains("\n## a.md\n"), "{out}");
        assert!(out.contains("\n## docs/b.md\n"), "{out}");
        assert!(out.contains("\n## docs/api/deep/c.md\n"), "{out}");

        // The deepest file still keeps its own heading hierarchy distinct.
        assert!(out.contains("\n### H1\n"), "{out}");
        assert!(out.contains("\n#### H2\n"), "{out}");
        assert!(out.contains("\n##### H3\n"), "{out}");
    }

    #[test]
    fn uses_the_frontmatter_title_for_the_file_heading() {
        let out = render(
            "N",
            &[section(
                "N",
                vec![file(
                    "docs/auth.md",
                    "---\ntitle: Authentication\n---\nbody\n",
                )],
            )],
            "t",
        );
        assert!(out.contains("## docs/auth.md\n"), "{out}");
        assert!(out.contains("### Authentication\n"), "{out}");
    }

    #[test]
    fn keeps_frontmatter_when_the_repo_opts_out() {
        let mut s = section("N", vec![file("a.md", "---\ntitle: T\n---\nbody\n")]);
        s.strip_frontmatter = false;
        let out = render("N", &[s], "t");
        assert!(out.contains("title: T"), "{out}");
        assert!(out.contains("## a.md\n"), "{out}");
    }

    #[test]
    fn records_provenance_for_each_repo() {
        let out = render("All", &[section("One", vec![file("a.md", "x")])], "t");
        assert!(out.contains("at commit `01234567`"), "{out}");
        assert!(out.contains("(1 file)"), "{out}");
    }

    #[test]
    fn rendering_is_deterministic() {
        let sections = [section("One", vec![file("docs/a.md", "# A")])];
        assert_eq!(
            render("N", &sections, "fixed"),
            render("N", &sections, "fixed")
        );
    }

    #[test]
    fn snapshot_of_a_full_document() {
        let sections = vec![
            section(
                "Personal Notes",
                vec![
                    file("README.md", "# Overview\n\nTop level notes.\n"),
                    file(
                        "docs/setup.md",
                        "---\ntitle: Getting Started\n---\n\nSteps\n=====\n\n```sh\n# not a heading\nmake\n```\n",
                    ),
                    file(
                        "docs/api/auth.md",
                        "# Auth\n\n## Tokens\n\n![diagram](./flow.png)\n",
                    ),
                ],
            ),
            section(
                "Work Notes",
                vec![file("standup.md", "## Monday\n\nnotes\n")],
            ),
        ];

        insta::assert_snapshot!(render(
            "Notes — All Repos",
            &sections,
            "2026-01-01 00:00 UTC"
        ));
    }
}
