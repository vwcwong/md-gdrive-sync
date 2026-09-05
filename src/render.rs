//! Turning collected Markdown into a single document whose heading outline
//! mirrors the original directory structure.
//!
//! Everything here is pure text in, text out: no filesystem, no network, no
//! clock. The generation timestamp is passed in so output is reproducible and
//! snapshot-testable.

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
/// A document holding a single repository omits the per-repository heading,
/// since the document title already names it; that also lifts everything inside
/// it one level, leaving more of the six available heading levels for deep
/// directory trees.
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
/// Six heading levels is not enough for a document title, a repository, a
/// directory tree and then the file's own headings: at two directories deep the
/// file's internal structure collapses into a run of H6 siblings. The path in a
/// heading conveys the directory structure losslessly, whereas squashed content
/// headings lose it for good, so the directory tree gives up its levels and
/// every file gets the same budget no matter how deep it sits.
fn push_section(out: &mut String, section: &Section, base: usize) {
    let level = base + 1;

    for file in &section.files {
        push_heading(out, level, &file.rel_path);

        let (frontmatter_title, body) = if section.strip_frontmatter {
            split_frontmatter(&file.content)
        } else {
            (None, file.content.as_str())
        };

        // A frontmatter title stands in for the H1 the file does not have, so
        // it sits at the level the body's own H1 would occupy.
        if let Some(title) = &frontmatter_title {
            push_heading(out, level + 1, title);
        }

        let rendered = transform_body(body.trim_start_matches(['\n', '\r']), level);
        out.push_str(rendered.trim_end());
        out.push_str("\n\n");
    }
}

fn push_heading(out: &mut String, level: usize, text: &str) {
    push_heading_line(out, level, text);
    out.push('\n');
}

// ---------------------------------------------------------------------------
// Body transformation
// ---------------------------------------------------------------------------

/// One classified source line.
enum Line<'a> {
    Heading(usize, &'a str),
    /// Inside a code block: passed through byte for byte.
    Code(&'a str),
    Text(&'a str),
}

/// Demotes the file's own headings so they nest under its path heading, and
/// replaces images with a text placeholder.
///
/// Heading levels are normalised rather than shifted by a fixed amount: a file
/// whose top heading is an H2 has it placed directly under the path heading
/// instead of leaving an empty level, which both tightens the Docs outline and
/// leaves more of the six levels for whatever nests below.
///
/// Code blocks are passed through untouched; without that, a `# comment` in a
/// shell snippet would be rewritten as a heading and corrupt both the snippet
/// and the document outline.
fn transform_body(body: &str, offset: usize) -> String {
    let lines = scan(body);

    let top = lines
        .iter()
        .filter_map(|l| match l {
            Line::Heading(level, _) => Some(*level),
            _ => None,
        })
        .min()
        .unwrap_or(1);

    let mut out = String::with_capacity(body.len());
    for line in &lines {
        match line {
            Line::Heading(level, text) => {
                push_heading_line(&mut out, level + offset + 1 - top, text);
            }
            Line::Code(raw) => {
                out.push_str(raw);
                out.push('\n');
            }
            Line::Text(raw) => {
                out.push_str(&replace_images(raw));
                out.push('\n');
            }
        }
    }
    out
}

/// Splits the body into headings, code and prose, tracking fences so that
/// nothing inside a code block is ever reinterpreted.
fn scan(body: &str) -> Vec<Line<'_>> {
    let lines: Vec<&str> = body.lines().collect();
    let mut out = Vec::with_capacity(lines.len());
    let mut fence: Option<Fence> = None;
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];

        if let Some(open) = &fence {
            out.push(Line::Code(line));
            if open.closed_by(line) {
                fence = None;
            }
            i += 1;
            continue;
        }

        if let Some(open) = Fence::opened_by(line) {
            fence = Some(open);
            out.push(Line::Code(line));
            i += 1;
            continue;
        }

        // Four spaces of indentation is an indented code block, and no heading
        // can be indented that far.
        if line.starts_with("    ") || line.starts_with('\t') {
            out.push(Line::Code(line));
            i += 1;
            continue;
        }

        if let Some((level, text)) = atx_heading(line) {
            out.push(Line::Heading(level, text));
            i += 1;
            continue;
        }

        // A setext heading is a line of text underlined by = or -. Treated as a
        // heading so it can be demoted like any other; left alone it would stay
        // an H1 or H2 and break the outline.
        if let Some(level) = lines.get(i + 1).and_then(|next| setext_underline(next))
            && can_be_setext_text(line)
        {
            out.push(Line::Heading(level, line.trim()));
            i += 2;
            continue;
        }

        out.push(Line::Text(line));
        i += 1;
    }

    out
}

fn push_heading_line(out: &mut String, level: usize, text: &str) {
    out.push_str(&"#".repeat(level.min(MAX_HEADING)));
    out.push(' ');
    out.push_str(text);
    out.push('\n');
}

/// `## Title ##` -> `(2, "Title")`
fn atx_heading(line: &str) -> Option<(usize, &str)> {
    // Up to three spaces of indentation still counts as a heading; four makes
    // it an indented code block.
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let trimmed = &line[indent..];

    let hashes = trimmed.len() - trimmed.trim_start_matches('#').len();
    if hashes == 0 || hashes > MAX_HEADING {
        return None;
    }

    let rest = &trimmed[hashes..];
    if !rest.is_empty() && !rest.starts_with(' ') && !rest.starts_with('\t') {
        return None; // `#hashtag`, not a heading
    }

    Some((hashes, strip_closing_hashes(rest.trim())))
}

/// Removes an optional closing `###` run, which only counts as a delimiter when
/// whitespace separates it from the text. Without that check a heading like
/// `## Notes on C#` would lose its last character.
fn strip_closing_hashes(text: &str) -> &str {
    let without = text.trim_end_matches('#');
    if without.len() == text.len() {
        return text;
    }
    if without.is_empty() || without.ends_with([' ', '\t']) {
        without.trim_end()
    } else {
        text
    }
}

/// Whether a line can be the text of a setext heading.
///
/// A `---` under a list item or table row is a thematic break rather than an
/// underline, and treating it as one would promote list text into a heading.
fn can_be_setext_text(line: &str) -> bool {
    let t = line.trim();
    if t.is_empty() {
        return false;
    }
    let list_or_quote = t.starts_with(['-', '*', '+', '>', '|', '#'])
        || t.split_once(['.', ')'])
            .is_some_and(|(head, _)| !head.is_empty() && head.chars().all(|c| c.is_ascii_digit()));
    !list_or_quote
}

/// A line of only `=` is an H1 underline, only `-` an H2 underline.
fn setext_underline(line: &str) -> Option<usize> {
    let t = line.trim();
    if t.len() >= 2 && t.chars().all(|c| c == '=') {
        Some(1)
    } else if t.len() >= 2 && t.chars().all(|c| c == '-') {
        Some(2)
    } else {
        None
    }
}

/// Drive turns Markdown images into base64 data URIs that render broken in a
/// Google Doc. The alt text is what carries meaning for NotebookLM anyway, so
/// keep that and drop the reference.
fn replace_images(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;

    while let Some(start) = rest.find("![") {
        let after = &rest[start + 2..];
        let Some(alt_end) = after.find(']') else {
            break;
        };
        let alt = &after[..alt_end];

        let tail = &after[alt_end + 1..];
        if !tail.starts_with('(') {
            out.push_str(&rest[..start + 2]);
            rest = after;
            continue;
        }
        let Some(url_end) = tail.find(')') else { break };

        out.push_str(&rest[..start]);
        out.push_str(&if alt.trim().is_empty() {
            "*[image]*".to_string()
        } else {
            format!("*[image: {alt}]*")
        });
        rest = &tail[url_end + 1..];
    }

    out.push_str(rest);
    out
}

struct Fence {
    marker: char,
    length: usize,
}

impl Fence {
    fn opened_by(line: &str) -> Option<Self> {
        let trimmed = line.trim_start();
        if line.len() - trimmed.len() > 3 {
            return None;
        }
        for marker in ['`', '~'] {
            let length = trimmed.len() - trimmed.trim_start_matches(marker).len();
            if length >= 3 {
                return Some(Fence { marker, length });
            }
        }
        None
    }

    /// A fence closes on a line of at least as many of the same character and
    /// nothing else.
    fn closed_by(&self, line: &str) -> bool {
        let trimmed = line.trim();
        trimmed.len() >= self.length && trimmed.chars().all(|c| c == self.marker)
    }
}

// ---------------------------------------------------------------------------
// Frontmatter
// ---------------------------------------------------------------------------

/// Splits leading `---` delimited frontmatter off the body, returning any
/// `title:` found in it.
fn split_frontmatter(content: &str) -> (Option<String>, &str) {
    let Some(rest) = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))
    else {
        return (None, content);
    };

    let Some(end) = find_frontmatter_end(rest) else {
        // No closing delimiter: it was an horizontal rule, not frontmatter.
        return (None, content);
    };

    let (frontmatter, body) = rest.split_at(end.0);
    (frontmatter_title(frontmatter), &body[end.1..])
}

/// Returns (offset of the closing delimiter, length of the delimiter line).
fn find_frontmatter_end(rest: &str) -> Option<(usize, usize)> {
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            return Some((offset, line.len()));
        }
        offset += line.len();
    }
    None
}

fn frontmatter_title(frontmatter: &str) -> Option<String> {
    for line in frontmatter.lines() {
        if let Some(value) = line.strip_prefix("title:") {
            let value = value.trim().trim_matches(['"', '\'']).trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

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

    // -- heading demotion ---------------------------------------------------

    #[test]
    fn demotes_headings_by_the_file_depth() {
        let out = transform_body("# One\n## Two\n", 3);
        assert_eq!(out, "#### One\n##### Two\n");
    }

    #[test]
    fn caps_demotion_at_six_levels() {
        // Six levels of the file's own headings cannot all survive under a
        // heading that is already at level 3.
        let out = transform_body("# 1\n## 2\n### 3\n#### 4\n##### 5\n###### 6\n", 3);
        assert!(out.ends_with("###### 5\n###### 6\n"), "{out}");
    }

    #[test]
    fn normalises_a_file_whose_top_heading_is_not_h1() {
        // Starts at H2, so H2 lands directly under the path heading rather than
        // leaving a gap, and the relative nesting is kept.
        let out = transform_body("## Top\n### Under\n", 2);
        assert_eq!(out, "### Top\n#### Under\n");
    }

    #[test]
    fn normalisation_uses_the_shallowest_heading_not_the_first() {
        let out = transform_body("### Third\n## Second\n", 1);
        assert_eq!(out, "### Third\n## Second\n");
    }

    #[test]
    fn leaves_hashes_inside_fenced_code_alone() {
        let body = "text\n\n```sh\n# not a heading\necho hi\n```\n\n# real heading\n";
        let out = transform_body(body, 2);
        assert!(out.contains("# not a heading"), "{out}");
        assert!(!out.contains("### not a heading"), "{out}");
        assert!(out.contains("### real heading"), "{out}");
    }

    #[test]
    fn handles_tilde_fences_and_longer_fences() {
        let body = "~~~\n# inside tilde\n~~~\n````\n# inside long\n````\n# outside\n";
        let out = transform_body(body, 1);
        assert!(out.contains("\n# inside tilde\n"), "{out}");
        assert!(out.contains("\n# inside long\n"), "{out}");
        assert!(out.contains("\n## outside\n"), "{out}");
    }

    #[test]
    fn a_shorter_run_does_not_close_a_longer_fence() {
        let body = "````\n```\n# still inside\n````\n# outside\n";
        let out = transform_body(body, 1);
        assert!(out.contains("\n# still inside\n"), "{out}");
        assert!(out.contains("\n## outside\n"), "{out}");
    }

    #[test]
    fn leaves_indented_code_blocks_alone() {
        let out = transform_body("    # indented code\n", 2);
        assert_eq!(out, "    # indented code\n");
    }

    #[test]
    fn does_not_treat_a_hashtag_as_a_heading() {
        let out = transform_body("#hashtag not a heading\n", 2);
        assert_eq!(out, "#hashtag not a heading\n");
    }

    #[test]
    fn keeps_a_trailing_hash_that_is_part_of_the_text() {
        assert_eq!(atx_heading("## Notes on C#"), Some((2, "Notes on C#")));
        assert_eq!(atx_heading("## Closed ##"), Some((2, "Closed")));
    }

    // -- setext headings ----------------------------------------------------

    #[test]
    fn rewrites_setext_headings_so_they_can_be_demoted() {
        let out = transform_body("Title\n=====\n\nSub\n---\n", 2);
        assert!(out.contains("### Title\n"), "{out}");
        assert!(out.contains("#### Sub\n"), "{out}");
    }

    #[test]
    fn does_not_mistake_a_thematic_break_for_an_underline() {
        let out = transform_body("- item\n---\n", 2);
        assert_eq!(out, "- item\n---\n");

        let out = transform_body("paragraph\n\n---\n\nmore\n", 2);
        assert!(out.contains("\n---\n"), "{out}");
    }

    #[test]
    fn does_not_mistake_a_table_separator_for_an_underline() {
        let out = transform_body("| a | b |\n|---|---|\n| 1 | 2 |\n", 2);
        assert_eq!(out, "| a | b |\n|---|---|\n| 1 | 2 |\n");
    }

    // -- images -------------------------------------------------------------

    #[test]
    fn replaces_images_with_their_alt_text() {
        assert_eq!(
            replace_images("before ![a diagram](./x.png) after"),
            "before *[image: a diagram]* after"
        );
        assert_eq!(replace_images("![](x.png)"), "*[image]*");
    }

    #[test]
    fn leaves_ordinary_links_alone() {
        let line = "see [the docs](https://example.invalid)";
        assert_eq!(replace_images(line), line);
    }

    #[test]
    fn does_not_replace_images_inside_code() {
        let out = transform_body("```\n![keep](x.png)\n```\n", 1);
        assert!(out.contains("![keep](x.png)"), "{out}");
    }

    // -- frontmatter --------------------------------------------------------

    #[test]
    fn strips_frontmatter_and_takes_the_title_from_it() {
        let (title, body) = split_frontmatter("---\ntitle: My Note\ntags: [a]\n---\n# Body\n");
        assert_eq!(title.as_deref(), Some("My Note"));
        assert_eq!(body, "# Body\n");
    }

    #[test]
    fn unquotes_a_frontmatter_title() {
        let (title, _) = split_frontmatter("---\ntitle: \"Quoted\"\n---\nbody\n");
        assert_eq!(title.as_deref(), Some("Quoted"));
    }

    #[test]
    fn leaves_a_leading_horizontal_rule_alone() {
        let content = "---\n\nnot frontmatter, no closing delimiter\n";
        let (title, body) = split_frontmatter(content);
        assert_eq!(title, None);
        assert_eq!(body, content);
    }

    #[test]
    fn frontmatter_without_a_title_still_gets_stripped() {
        let (title, body) = split_frontmatter("---\ntags: [a]\n---\nbody\n");
        assert_eq!(title, None);
        assert_eq!(body, "body\n");
    }

    // -- document structure -------------------------------------------------

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
