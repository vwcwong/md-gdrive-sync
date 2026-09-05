//! Loading and validation of `repos.yml`.
//!
//! Deserialization is deliberately strict (`deny_unknown_fields`) so a typo in
//! the config is a hard error at load time rather than a key that is silently
//! ignored and a document that quietly comes out wrong.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Google Docs tops out around 1.02M characters. Stay under it with room for
/// the headings and table of contents this tool adds on top of the source text.
const DEFAULT_MAX_CHARS: usize = 900_000;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not read config at {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{path} is not valid YAML: {message}")]
    Parse { path: PathBuf, message: String },

    #[error("environment variable ${var} is referenced by the config but is not set")]
    MissingEnv { var: String },

    #[error("unterminated ${{ ... }} reference in config")]
    UnterminatedEnv,

    #[error("config is invalid: {0}")]
    Invalid(String),
}

type Result<T> = std::result::Result<T, ConfigError>;

// ---------------------------------------------------------------------------
// On-disk shape
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    drive: DriveConfig,
    #[serde(default)]
    defaults: RepoDefaults,
    repos: Vec<RawRepo>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriveConfig {
    /// ID of the destination folder, i.e. the trailing path segment of its
    /// Drive URL.
    pub folder_id: String,

    #[serde(default = "default_combined_doc_name")]
    pub combined_doc_name: String,

    #[serde(default = "yes")]
    pub emit_combined: bool,

    #[serde(default = "yes")]
    pub emit_per_repo: bool,

    /// Trash documents this tool created that no longer map to a config entry.
    #[serde(default)]
    pub prune_orphans: bool,

    #[serde(default = "default_max_chars")]
    pub max_chars_per_doc: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RepoDefaults {
    #[serde(default)]
    branch: Option<String>,
    #[serde(default = "default_include")]
    include: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(default = "yes")]
    strip_frontmatter: bool,
}

impl Default for RepoDefaults {
    // Hand-written rather than derived: the derived impl would give an empty
    // `include`, which silently collects nothing when `defaults:` is omitted.
    fn default() -> Self {
        Self {
            branch: None,
            include: default_include(),
            exclude: Vec::new(),
            strip_frontmatter: true,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRepo {
    url: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    private: bool,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    subdir: Option<String>,
    #[serde(default)]
    include: Option<Vec<String>>,
    #[serde(default)]
    exclude: Option<Vec<String>>,
    #[serde(default)]
    strip_frontmatter: Option<bool>,
}

fn yes() -> bool {
    true
}

fn default_combined_doc_name() -> String {
    "Notes — All Repos".to_string()
}

fn default_max_chars() -> usize {
    DEFAULT_MAX_CHARS
}

fn default_include() -> Vec<String> {
    vec!["**/*.md".to_string(), "**/*.markdown".to_string()]
}

// ---------------------------------------------------------------------------
// Resolved shape
// ---------------------------------------------------------------------------

/// A config with per-repo defaults already folded in, so nothing downstream
/// needs to know that `defaults:` exists.
#[derive(Debug, Clone)]
pub struct Config {
    pub drive: DriveConfig,
    pub repos: Vec<Repo>,
}

#[derive(Debug, Clone)]
pub struct Repo {
    pub url: String,
    pub name: String,
    pub private: bool,
    pub branch: Option<String>,
    pub subdir: Option<PathBuf>,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub strip_frontmatter: bool,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_yaml(&text, path)
    }

    pub fn from_yaml(text: &str, path: &Path) -> Result<Self> {
        let expanded = expand_env(text)?;

        let raw: RawConfig = serde_saphyr::from_str(&expanded).map_err(|e| ConfigError::Parse {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;

        let repos = raw
            .repos
            .into_iter()
            .map(|r| resolve_repo(r, &raw.defaults))
            .collect::<Result<Vec<_>>>()?;

        let config = Config {
            drive: raw.drive,
            repos,
        };
        config.validate()?;
        Ok(config)
    }

    /// Everything that can be checked without network access. Called on load,
    /// and on its own by `mdsync validate`.
    fn validate(&self) -> Result<()> {
        if self.drive.folder_id.trim().is_empty() {
            return Err(ConfigError::Invalid(
                "drive.folder_id is empty; set it to the destination folder's Drive ID".into(),
            ));
        }

        if self.drive.max_chars_per_doc == 0 {
            return Err(ConfigError::Invalid(
                "drive.max_chars_per_doc must be greater than zero".into(),
            ));
        }

        if !self.drive.emit_combined && !self.drive.emit_per_repo {
            return Err(ConfigError::Invalid(
                "drive.emit_combined and drive.emit_per_repo are both false, so the run \
                 would produce no documents"
                    .into(),
            ));
        }

        if self.repos.is_empty() {
            return Err(ConfigError::Invalid("repos is empty".into()));
        }

        let mut seen: HashSet<&str> = HashSet::new();
        for repo in &self.repos {
            if !seen.insert(repo.name.as_str()) {
                return Err(ConfigError::Invalid(format!(
                    "two repositories resolve to the name {:?}; set an explicit `name:` on one \
                     of them, since the name is used as the Drive document name",
                    repo.name
                )));
            }
            repo.validate()?;
        }

        Ok(())
    }
}

impl Repo {
    fn validate(&self) -> Result<()> {
        // The PAT is injected as HTTP basic auth, which only works over https.
        // An ssh remote would need a deploy key on the runner instead.
        if !self.url.starts_with("https://") && !self.url.starts_with("http://") {
            return Err(ConfigError::Invalid(format!(
                "{}: url must be http(s); ssh remotes are not supported because private \
                 repositories authenticate with a token, not a key",
                self.name
            )));
        }

        if self.name.trim().is_empty() {
            return Err(ConfigError::Invalid(format!("{}: name is empty", self.url)));
        }

        if let Some(subdir) = &self.subdir
            && (subdir.is_absolute() || subdir.components().any(|c| c.as_os_str() == ".."))
        {
            return Err(ConfigError::Invalid(format!(
                "{}: subdir must be a relative path inside the repository, got {:?}",
                self.name, subdir
            )));
        }

        // Compile the globs now so a bad pattern fails at `validate` time rather
        // than midway through a scheduled run.
        for pattern in self.include.iter().chain(self.exclude.iter()) {
            globset::Glob::new(pattern).map_err(|e| {
                ConfigError::Invalid(format!("{}: invalid glob {:?}: {e}", self.name, pattern))
            })?;
        }

        if self.include.is_empty() {
            return Err(ConfigError::Invalid(format!(
                "{}: include is empty, so no files would ever be collected",
                self.name
            )));
        }

        Ok(())
    }
}

fn resolve_repo(raw: RawRepo, defaults: &RepoDefaults) -> Result<Repo> {
    let name = match raw.name {
        Some(n) => n,
        None => repo_name_from_url(&raw.url).ok_or_else(|| {
            ConfigError::Invalid(format!(
                "could not derive a name from url {:?}; set `name:` explicitly",
                raw.url
            ))
        })?,
    };

    // `include` replaces the default when given: a repo that opts into a
    // narrower set of files means it, and inheriting `**/*.md` would defeat it.
    // `exclude` is additive, because per-repo excludes are almost always extra
    // rules on top of the global ones rather than a replacement for them.
    let include = raw.include.unwrap_or_else(|| defaults.include.clone());
    let mut exclude = defaults.exclude.clone();
    exclude.extend(raw.exclude.unwrap_or_default());

    Ok(Repo {
        url: raw.url,
        name,
        private: raw.private,
        branch: raw.branch.or_else(|| defaults.branch.clone()),
        subdir: raw.subdir.map(PathBuf::from),
        include,
        exclude,
        strip_frontmatter: raw.strip_frontmatter.unwrap_or(defaults.strip_frontmatter),
    })
}

/// `https://github.com/owner/notes.git` -> `notes`
fn repo_name_from_url(url: &str) -> Option<String> {
    let trimmed = url.trim_end_matches('/');
    let last = trimmed.rsplit('/').next()?;
    let last = last.strip_suffix(".git").unwrap_or(last);
    if last.is_empty() {
        None
    } else {
        Some(last.to_string())
    }
}

/// Substitutes `${VAR}` from the environment.
///
/// Only bare identifiers are treated as references, so glob patterns and
/// document names containing braces are left alone.
fn expand_env(text: &str) -> Result<String> {
    expand_env_with(text, |var| std::env::var(var).ok())
}

/// The lookup is injected so tests do not have to mutate the process
/// environment, which is `unsafe` under edition 2024 and forbidden in this crate.
fn expand_env_with(text: &str, lookup: impl Fn(&str) -> Option<String>) -> Result<String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after.find('}').ok_or(ConfigError::UnterminatedEnv)?;
        let var = &after[..end];

        if is_env_identifier(var) {
            let value = lookup(var).ok_or_else(|| ConfigError::MissingEnv {
                var: var.to_string(),
            })?;
            out.push_str(&value);
        } else {
            // Not a variable reference; emit it untouched.
            out.push_str("${");
            out.push_str(var);
            out.push('}');
        }

        rest = &after[end + 1..];
    }

    out.push_str(rest);
    Ok(out)
}

fn is_env_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(yaml: &str) -> Result<Config> {
        Config::from_yaml(yaml, Path::new("repos.yml"))
    }

    const MINIMAL: &str = r#"
drive:
  folder_id: "abc123"
repos:
  - url: https://github.com/owner/notes
"#;

    #[test]
    fn applies_defaults_when_defaults_block_is_absent() {
        let config = parse(MINIMAL).unwrap();
        let repo = &config.repos[0];

        assert_eq!(repo.include, vec!["**/*.md", "**/*.markdown"]);
        assert!(repo.exclude.is_empty());
        assert!(repo.strip_frontmatter);
        assert!(!repo.private);
        assert_eq!(repo.branch, None);

        assert!(config.drive.emit_combined);
        assert!(config.drive.emit_per_repo);
        assert!(!config.drive.prune_orphans);
        assert_eq!(config.drive.max_chars_per_doc, DEFAULT_MAX_CHARS);
    }

    #[test]
    fn derives_repo_name_from_url() {
        assert_eq!(parse(MINIMAL).unwrap().repos[0].name, "notes");

        let with_git_suffix = r#"
drive:
  folder_id: "abc123"
repos:
  - url: https://github.com/owner/work-notes.git/
"#;
        assert_eq!(parse(with_git_suffix).unwrap().repos[0].name, "work-notes");
    }

    #[test]
    fn explicit_include_replaces_default_but_exclude_is_additive() {
        let yaml = r#"
drive:
  folder_id: "abc123"
defaults:
  include: ["**/*.md"]
  exclude: ["node_modules/**"]
repos:
  - url: https://github.com/owner/notes
    include: ["docs/**/*.md"]
    exclude: ["drafts/**"]
"#;
        let repo = &parse(yaml).unwrap().repos[0];
        assert_eq!(repo.include, vec!["docs/**/*.md"]);
        assert_eq!(repo.exclude, vec!["node_modules/**", "drafts/**"]);
    }

    #[test]
    fn repo_overrides_default_branch_and_frontmatter() {
        let yaml = r#"
drive:
  folder_id: "abc123"
defaults:
  branch: main
  strip_frontmatter: true
repos:
  - url: https://github.com/owner/a
  - url: https://github.com/owner/b
    branch: notes
    strip_frontmatter: false
"#;
        let config = parse(yaml).unwrap();
        assert_eq!(config.repos[0].branch.as_deref(), Some("main"));
        assert!(config.repos[0].strip_frontmatter);
        assert_eq!(config.repos[1].branch.as_deref(), Some("notes"));
        assert!(!config.repos[1].strip_frontmatter);
    }

    #[test]
    fn rejects_unknown_keys() {
        let yaml = r#"
drive:
  folder_id: "abc123"
repos:
  - url: https://github.com/owner/notes
    inclde: ["**/*.md"]
"#;
        assert!(matches!(parse(yaml), Err(ConfigError::Parse { .. })));
    }

    #[test]
    fn rejects_duplicate_repo_names() {
        let yaml = r#"
drive:
  folder_id: "abc123"
repos:
  - url: https://github.com/owner/notes
  - url: https://gitlab.com/other/notes
"#;
        let err = parse(yaml).unwrap_err().to_string();
        assert!(err.contains("resolve to the name"), "{err}");
    }

    #[test]
    fn rejects_ssh_urls() {
        let yaml = r#"
drive:
  folder_id: "abc123"
repos:
  - url: git@github.com:owner/notes.git
"#;
        let err = parse(yaml).unwrap_err().to_string();
        assert!(err.contains("must be http(s)"), "{err}");
    }

    #[test]
    fn rejects_subdir_escaping_the_repo() {
        let yaml = r#"
drive:
  folder_id: "abc123"
repos:
  - url: https://github.com/owner/notes
    subdir: "../../etc"
"#;
        let err = parse(yaml).unwrap_err().to_string();
        assert!(err.contains("relative path inside"), "{err}");
    }

    #[test]
    fn rejects_invalid_glob() {
        let yaml = r#"
drive:
  folder_id: "abc123"
repos:
  - url: https://github.com/owner/notes
    include: ["docs/[unclosed"]
"#;
        let err = parse(yaml).unwrap_err().to_string();
        assert!(err.contains("invalid glob"), "{err}");
    }

    #[test]
    fn rejects_config_that_would_emit_nothing() {
        let yaml = r#"
drive:
  folder_id: "abc123"
  emit_combined: false
  emit_per_repo: false
repos:
  - url: https://github.com/owner/notes
"#;
        let err = parse(yaml).unwrap_err().to_string();
        assert!(err.contains("no documents"), "{err}");
    }

    #[test]
    fn rejects_empty_repo_list() {
        let yaml = r#"
drive:
  folder_id: "abc123"
repos: []
"#;
        assert!(
            parse(yaml)
                .unwrap_err()
                .to_string()
                .contains("repos is empty")
        );
    }

    #[test]
    fn expands_env_references() {
        let out = expand_env_with("folder_id: ${MY_FOLDER}", |v| {
            (v == "MY_FOLDER").then(|| "xyz".to_string())
        })
        .unwrap();
        assert_eq!(out, "folder_id: xyz");
    }

    #[test]
    fn missing_env_reference_names_the_variable() {
        let err = expand_env_with("${NOPE}", |_| None).unwrap_err();
        assert!(matches!(err, ConfigError::MissingEnv { ref var } if var == "NOPE"));
    }

    #[test]
    fn leaves_non_identifier_braces_alone() {
        // Braces show up in glob alternations; they must survive untouched.
        let input = "include: [\"**/*.${md,markdown}\"]";
        let out = expand_env_with(input, |_| panic!("should not be looked up")).unwrap();
        assert_eq!(out, input);
    }

    #[test]
    fn detects_unterminated_env_reference() {
        assert!(matches!(
            expand_env_with("${OPEN", |_| None),
            Err(ConfigError::UnterminatedEnv)
        ));
    }
}
