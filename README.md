# md-gdrive-sync

Clones a list of git repos, collects their Markdown, and publishes it to a Google
Drive folder as Google Docs — one per repo plus a combined doc — for use as
NotebookLM sources.

Re-running updates the existing docs rather than replacing them, so NotebookLM
sources you have already added keep working.

## Output

Each file becomes a section headed by its full path, with the file's own
headings nested underneath:

```
## docs/api/auth.md
### Auth
#### Tokens
```

Frontmatter is stripped, with a `title:` used as the heading. Code blocks are
left alone. Images become a text placeholder. Anything covered by `.gitignore`,
and dotfiles, are skipped.

## Setup

```sh
direnv allow        # or: nix develop
cargo test
```

**Google Cloud** — create a project, enable the Drive API, then:

1. Publish the OAuth consent screen. Left in *Testing*, the refresh token
   expires every 7 days.
2. Create an OAuth client ID of type **Desktop app**. A service account will
   not work.

**Refresh token**

```sh
export GOOGLE_CLIENT_ID=... GOOGLE_CLIENT_SECRET=...
cargo run -- auth
```

**Configure** — edit `repos.yml`, then
`GDRIVE_FOLDER_ID=<id> cargo run -- validate`. The folder ID is the last segment
of the Drive folder's URL.

**Secrets** — set `GDRIVE_FOLDER_ID`, `GOOGLE_CLIENT_ID`,
`GOOGLE_CLIENT_SECRET`, `GOOGLE_REFRESH_TOKEN`, and `NOTES_REPO_TOKEN` (a
GitHub token that can clone private repos; omit if all repos are public)
under *Settings → Secrets and variables → Actions*.

**First run** — trigger the workflow manually with *dry run* ticked. Everything
is rendered and left as a downloadable artifact without touching Drive. Then run
it for real and leave the daily schedule to it.

## Commands

```sh
cargo run -- validate                       # check repos.yml, no network
cargo run -- sync --dry-run                 # render to ./out, skip Drive
cargo run -- sync                           # render and publish
cargo run -- sync --only "Personal Notes"   # one repo (skips pruning)
cargo run -- auth                           # mint a refresh token
```

Every run leaves its rendered Markdown in `./out`, published or not.

## Notes

- A repo-level `include` **replaces** `defaults.include`; a repo-level `exclude`
  is **added to** `defaults.exclude`.
- `prune_orphans` trashes docs that no longer match a configured repo. Skipped
  under `--only`.
- The refresh token does not need rotating. Re-run `cargo run -- auth` if it is
  ever revoked.
- A repo collecting 0 files usually means its globs or `.gitignore`.
