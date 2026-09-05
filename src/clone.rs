//! Shallow-cloning the configured repositories into throwaway directories.
//!
//! Shells out to `git` rather than binding libgit2 or gitoxide: shallow clone
//! with token auth is well-trodden ground for the CLI, and `git` is already a
//! declared dependency of the dev shell and the CI image.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use tempfile::TempDir;
use tracing::{debug, info};

use crate::config::Repo;

/// Environment variable holding the PAT used for repositories marked private.
pub const TOKEN_VAR: &str = "NOTES_REPO_TOKEN";

/// A cloned repository. Deleting this deletes the working tree.
#[derive(Debug)]
pub struct Checkout {
    /// Config name of the repository this came from.
    pub name: String,
    /// The directory to collect Markdown from: the clone root, or the
    /// configured `subdir` beneath it.
    pub walk_root: PathBuf,
    /// Full SHA of the commit that was checked out, recorded in the rendered
    /// document so an answer can be traced back to a revision.
    pub commit: String,

    _dir: TempDir,
}

pub struct Cloner {
    /// PAT used for repositories marked `private: true`.
    token: Option<String>,
}

impl Cloner {
    pub fn new(token: Option<String>) -> Self {
        Self {
            token: token.filter(|t| !t.trim().is_empty()),
        }
    }

    pub fn checkout(&self, repo: &Repo) -> Result<Checkout> {
        if repo.private && self.token.is_none() {
            bail!(
                "{} is marked private but no token was supplied; set {TOKEN_VAR}",
                repo.name
            );
        }

        let dir = tempfile::Builder::new()
            .prefix("mdsync-")
            .tempdir()
            .context("creating a temporary directory for the clone")?;
        let root = dir.path().join("repo");

        info!(repo = %repo.name, "cloning");
        self.run_clone(repo, &root)?;

        let commit = git_stdout(&root, &["rev-parse", "HEAD"])
            .with_context(|| format!("resolving HEAD of {}", repo.name))?;

        let walk_root = match &repo.subdir {
            Some(subdir) => {
                let path = root.join(subdir);
                if !path.is_dir() {
                    bail!(
                        "{}: subdir {} does not exist in the repository",
                        repo.name,
                        subdir.display()
                    );
                }
                path
            }
            None => root,
        };

        debug!(repo = %repo.name, %commit, "cloned");
        Ok(Checkout {
            name: repo.name.clone(),
            walk_root,
            commit,
            _dir: dir,
        })
    }

    fn run_clone(&self, repo: &Repo, into: &Path) -> Result<()> {
        let mut args: Vec<String> = vec![
            "clone".into(),
            "--depth".into(),
            "1".into(),
            "--single-branch".into(),
            "--no-tags".into(),
            "--quiet".into(),
        ];
        if let Some(branch) = &repo.branch {
            args.push("--branch".into());
            args.push(branch.clone());
        }
        args.push(clone_url(&repo.url, self.use_token_for(repo)));
        args.push(into.display().to_string());

        let mut command = Command::new("git");
        command.args(&args);
        // Never let git stop for input; a wrong or missing token should fail the
        // run rather than hang a scheduled job forever.
        command.env("GIT_TERMINAL_PROMPT", "0");

        // The token is handed over through an askpass helper rather than being
        // embedded in the URL. That keeps it out of the process argument list
        // and out of the clone's .git/config.
        let _askpass = if self.use_token_for(repo) {
            let token = self.token.as_deref().expect("checked by use_token_for");
            let helper = Askpass::new()?;
            command.env("GIT_ASKPASS", helper.path());
            command.env(ASKPASS_TOKEN_VAR, token);
            Some(helper)
        } else {
            None
        };

        let output = command
            .output()
            .context("failed to run `git`; is it on PATH?")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "cloning {} failed ({}): {}",
                repo.name,
                output.status,
                redact(stderr.trim(), self.token.as_deref())
            );
        }

        Ok(())
    }

    fn use_token_for(&self, repo: &Repo) -> bool {
        repo.private && self.token.is_some()
    }
}

/// Adds the username git needs in order to ask askpass for a password. The
/// token itself is never placed in the URL.
fn clone_url(url: &str, authenticated: bool) -> String {
    if !authenticated {
        return url.to_string();
    }
    match url.split_once("://") {
        Some((scheme, rest)) => format!("{scheme}://x-access-token@{rest}"),
        None => url.to_string(),
    }
}

/// Replaces the token with a placeholder anywhere it appears, so a git error
/// message can be surfaced without leaking the credential into logs.
fn redact(text: &str, token: Option<&str>) -> String {
    match token {
        Some(token) if !token.is_empty() => text.replace(token, "***"),
        _ => text.to_string(),
    }
}

/// Tells GitHub Actions to scrub a value from logs. A no-op elsewhere.
///
/// Actions already masks values it injected from `secrets.*`; this covers
/// tokens that reach the process some other way.
pub fn mask_in_actions(secret: &str) {
    if !secret.is_empty() && std::env::var_os("GITHUB_ACTIONS").is_some() {
        println!("::add-mask::{secret}");
    }
}

const ASKPASS_TOKEN_VAR: &str = "MDSYNC_GIT_TOKEN";

/// A short-lived executable that echoes the token when git asks for a password.
struct Askpass {
    _dir: TempDir,
    path: PathBuf,
}

impl Askpass {
    fn new() -> Result<Self> {
        let dir = tempfile::Builder::new()
            .prefix("mdsync-askpass-")
            .tempdir()
            .context("creating a temporary directory for the askpass helper")?;
        let path = dir.path().join("askpass.sh");

        // Reads the token from the environment rather than baking it into the
        // file, so it never touches the disk.
        std::fs::write(
            &path,
            format!("#!/bin/sh\nprintf '%s' \"${ASKPASS_TOKEN_VAR}\"\n"),
        )
        .context("writing the askpass helper")?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .context("making the askpass helper executable")?;
        }

        Ok(Self { _dir: dir, path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

fn git_stdout(dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .context("failed to run `git`; is it on PATH?")?;

    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(url: &str) -> Repo {
        Repo {
            url: url.to_string(),
            name: "fixture".into(),
            private: false,
            branch: None,
            subdir: None,
            include: vec!["**/*.md".into()],
            exclude: vec![],
            strip_frontmatter: true,
        }
    }

    /// Builds a real repository on disk so the clone path can be exercised
    /// without network access.
    fn fixture_repo() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();

        let git = |args: &[&str]| {
            let status = Command::new("git")
                .arg("-C")
                .arg(path)
                .args(args)
                .output()
                .unwrap();
            assert!(
                status.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&status.stderr)
            );
        };

        Command::new("git")
            .args(["init", "-b", "main", "--quiet"])
            .arg(path)
            .output()
            .unwrap();
        git(&["config", "user.name", "Test"]);
        git(&["config", "user.email", "test@example.invalid"]);

        std::fs::create_dir_all(path.join("docs")).unwrap();
        std::fs::write(path.join("README.md"), "# Readme\n").unwrap();
        std::fs::write(path.join("docs/guide.md"), "# Guide\n").unwrap();

        git(&["add", "-A"]);
        git(&["commit", "-m", "initial", "--quiet"]);

        dir
    }

    #[test]
    fn adds_username_only_when_authenticating() {
        assert_eq!(
            clone_url("https://github.com/owner/repo", false),
            "https://github.com/owner/repo"
        );
        assert_eq!(
            clone_url("https://github.com/owner/repo", true),
            "https://x-access-token@github.com/owner/repo"
        );
    }

    #[test]
    fn redacts_the_token_from_error_text() {
        let text = "fatal: could not read Password for 'https://x@github.com': ghp_secret";
        let out = redact(text, Some("ghp_secret"));
        assert!(!out.contains("ghp_secret"), "{out}");
        assert!(out.ends_with("***"), "{out}");
    }

    #[test]
    fn private_repo_without_a_token_fails_before_running_git() {
        let mut r = repo("https://github.com/owner/repo");
        r.private = true;

        let err = Cloner::new(None).checkout(&r).unwrap_err().to_string();
        assert!(err.contains("NOTES_REPO_TOKEN"), "{err}");
    }

    #[test]
    fn blank_token_is_treated_as_absent() {
        let mut r = repo("https://github.com/owner/repo");
        r.private = true;

        let err = Cloner::new(Some("   ".into()))
            .checkout(&r)
            .unwrap_err()
            .to_string();
        assert!(err.contains("NOTES_REPO_TOKEN"), "{err}");
    }

    #[test]
    fn clones_a_local_repository_and_resolves_the_commit() {
        let source = fixture_repo();
        // file:// rather than a bare path: git only honours --depth over a
        // real transport.
        let url = format!("file://{}", source.path().display());

        let checkout = Cloner::new(None).checkout(&repo(&url)).unwrap();

        assert!(checkout.walk_root.join("README.md").is_file());
        assert!(checkout.walk_root.join("docs/guide.md").is_file());
        assert_eq!(checkout.commit.len(), 40, "{}", checkout.commit);
        assert!(checkout.commit.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn subdir_narrows_the_walk_root() {
        let source = fixture_repo();
        let mut r = repo(&format!("file://{}", source.path().display()));
        r.subdir = Some(PathBuf::from("docs"));

        let checkout = Cloner::new(None).checkout(&r).unwrap();

        assert!(checkout.walk_root.ends_with("docs"));
        assert!(checkout.walk_root.join("guide.md").is_file());
    }

    #[test]
    fn missing_subdir_is_reported_against_the_repo_name() {
        let source = fixture_repo();
        let mut r = repo(&format!("file://{}", source.path().display()));
        r.subdir = Some(PathBuf::from("nope"));

        let err = Cloner::new(None).checkout(&r).unwrap_err().to_string();
        assert!(err.contains("fixture: subdir nope does not exist"), "{err}");
    }

    #[test]
    fn missing_branch_surfaces_the_git_error() {
        let source = fixture_repo();
        let mut r = repo(&format!("file://{}", source.path().display()));
        r.branch = Some("does-not-exist".into());

        let err = Cloner::new(None).checkout(&r).unwrap_err().to_string();
        assert!(err.contains("cloning fixture failed"), "{err}");
    }
}
