//! Shallow-cloning the configured repositories into throwaway directories.
//!
//! Uses `gix` rather than driving the `git` binary: the clone is described by a
//! builder instead of an argument vector, the token is handed to the transport
//! through a callback instead of an askpass script, and nothing depends on a
//! `git` installation being on `PATH`.

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result, anyhow, bail};
use gix::credentials::helper::Action;
use gix::credentials::protocol;
use gix::remote::fetch::{Shallow, Tags};
use gix::sec::identity::Account;
use tempfile::TempDir;
use tracing::{debug, info};

use crate::config::Repo;

/// Environment variable holding the PAT used for repositories marked private.
pub const TOKEN_VAR: &str = "NOTES_REPO_TOKEN";

/// The username a PAT is presented under; GitHub ignores it but wants one.
const TOKEN_USERNAME: &str = "x-access-token";

/// A cloned repository. Deleting this deletes the working tree.
#[derive(Debug)]
pub struct Checkout {
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
        let commit = self
            .clone_into(repo, &root)
            .map_err(|err| anyhow!(redact(&format!("{err:#}"), self.token.as_deref())))
            .with_context(|| format!("cloning {} failed", repo.name))?;

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
            walk_root,
            commit,
            _dir: dir,
        })
    }

    /// Clones `repo` into `into`, returning the full SHA that was checked out.
    #[allow(clippy::result_large_err, reason = "the credential Result is gix's")]
    fn clone_into(&self, repo: &Repo, into: &Path) -> Result<String> {
        // Isolated options ignore the ambient git configuration and
        // environment, so a scheduled run behaves the same wherever it lands.
        let mut prepare = gix::clone::PrepareFetch::new(
            repo.url.as_str(),
            into,
            gix::create::Kind::WithWorktree,
            gix::create::Options::default(),
            gix::open::Options::isolated(),
        )?
        .with_shallow(Shallow::DepthAtRemote(NonZeroU32::MIN))
        .with_ref_name(repo.branch.as_deref())?
        // A clone otherwise fetches every tag, which for an old repository is
        // hundreds of commits of history the sync never looks at.
        .configure_remote(|remote| Ok(remote.with_fetch_tags(Tags::None)));

        // The token answers a credential request rather than riding along in
        // the URL, so it stays out of the clone's .git/config. Repositories
        // that need no token get a helper that refuses, which fails the run
        // instead of stalling on a prompt.
        let credentials = self.credentials_for(repo);
        prepare = prepare.configure_connection(move |connection| {
            let credentials = credentials.clone();
            connection.set_credentials(move |action| credentials.respond(action));
            Ok(())
        });

        let interrupt = AtomicBool::new(false);
        let (mut checkout, _) = prepare.fetch_then_checkout(gix::progress::Discard, &interrupt)?;
        let (cloned, _) = checkout.main_worktree(gix::progress::Discard, &interrupt)?;

        Ok(cloned.head_id()?.to_string())
    }

    fn credentials_for(&self, repo: &Repo) -> Credentials {
        match &self.token {
            Some(token) if repo.private => Credentials::Token(token.clone()),
            _ => Credentials::Refuse,
        }
    }
}

/// How to answer a server that asks the clone to authenticate.
#[derive(Clone)]
enum Credentials {
    Token(String),
    Refuse,
}

impl Credentials {
    #[allow(clippy::result_large_err, reason = "the credential Result is gix's")]
    fn respond(&self, action: Action) -> protocol::Result {
        match self {
            Credentials::Token(token) => match action {
                Action::Get(context) => Ok(Some(protocol::Outcome {
                    identity: Account {
                        username: TOKEN_USERNAME.into(),
                        password: token.clone(),
                        oauth_refresh_token: None,
                    },
                    next: context.into(),
                })),
                // Storing or erasing a credential we invented has no meaning.
                Action::Store(_) | Action::Erase(_) => Ok(None),
            },
            Credentials::Refuse => Err(protocol::Error::Quit),
        }
    }
}

/// Replaces the token with a placeholder anywhere it appears, so a clone error
/// can be surfaced without leaking the credential into logs.
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

#[cfg(test)]
mod tests {
    use std::process::Command;

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
    /// without network access. The `git` binary is only used here: cloning a
    /// local path speaks the wire protocol to `git-upload-pack` either way.
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
    fn redacts_the_token_from_error_text() {
        let text = "fatal: could not read Password for 'https://x@github.com': ghp_secret";
        let out = redact(text, Some("ghp_secret"));
        assert!(!out.contains("ghp_secret"), "{out}");
        assert!(out.ends_with("***"), "{out}");
    }

    #[test]
    #[allow(clippy::result_large_err, reason = "the credential Result is gix's")]
    fn a_token_is_offered_only_to_private_repositories() {
        let cloner = Cloner::new(Some("ghp_secret".into()));
        let mut private = repo("https://github.com/owner/repo");
        private.private = true;

        let identity = |repo: &Repo| {
            let context = protocol::Context {
                url: Some("https://github.com/owner/repo".into()),
                ..Default::default()
            };
            cloner
                .credentials_for(repo)
                .respond(Action::Get(context))
                .map(|outcome| outcome.map(|o| o.identity))
        };

        let account = identity(&private).unwrap().unwrap();
        assert_eq!(account.username, TOKEN_USERNAME);
        assert_eq!(account.password, "ghp_secret");

        assert!(matches!(
            identity(&repo("https://github.com/owner/repo")),
            Err(protocol::Error::Quit)
        ));
    }

    #[test]
    fn private_repo_without_a_token_fails_before_cloning() {
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
    fn missing_branch_surfaces_the_clone_error() {
        let source = fixture_repo();
        let mut r = repo(&format!("file://{}", source.path().display()));
        r.branch = Some("does-not-exist".into());

        let err = Cloner::new(None).checkout(&r).unwrap_err().to_string();
        assert!(err.contains("cloning fixture failed"), "{err}");
    }
}
