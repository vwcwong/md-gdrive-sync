# md-gdrive-sync

Clones a list of git repos, collects their Markdown, and publishes it to a Google
Drive folder as Google Docs — one per repo plus a combined doc — for use as
NotebookLM sources.

Docs are **updated in place**, so Drive IDs never change and NotebookLM sources
stay linked across runs.

## Output

Each file becomes one heading carrying its full path, with the file's own
headings nested beneath:

```
## docs/api/auth.md      <- the file, path intact
### Auth                 <- its own H1
#### Tokens              <- its own H2
```

A heading per directory level would exhaust Markdown's six levels before a deep
file's own structure could be represented; the full path keeps the directory
structure lossless and gives every file the same budget.

Per file: frontmatter stripped (`title:` becomes the top heading), setext
headings rewritten, code blocks untouched, images replaced with
`*[image: alt]*`. `.gitignore`d files and dotfiles are skipped.

## Setup

```sh
direnv allow        # or: nix develop
cargo test
```

**Google Cloud** — create a project, enable the Drive API, then:

1. OAuth consent screen: *External*, scope
   `https://www.googleapis.com/auth/drive.file`, publishing status
   **"In production"**. That scope is non-sensitive so this needs no
   verification review, and it is what stops the refresh token expiring after 7
   days in *Testing*.
2. Create an OAuth client ID of type **Desktop app**.

> Not a service account — Google's docs state they ["don't have storage quota
> and can't own any files"](https://developers.google.com/workspace/drive/api/guides/handle-errors),
> so one cannot write to a personal Drive folder even when it is shared with it.

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
fine-grained PAT with read-only Contents access; omit if all repos are public)
under *Settings → Secrets and variables → Actions*.

**First run** — trigger the workflow manually with *dry run* ticked. That
exercises the build, cloning and private-repo auth, and uploads the rendered
Markdown as an artifact, without touching Drive. Then run it for real and leave
the daily schedule to it.

## Commands

```sh
cargo run -- validate                       # check repos.yml, no network
cargo run -- sync --dry-run                 # render to ./out, skip Drive
cargo run -- sync                           # render and publish
cargo run -- sync --only "Personal Notes"   # one repo (skips pruning)
cargo run -- auth                           # mint a refresh token
```

`sync` writes to `./out` on every run, so there is always a local artifact to diff.

## Notes

- A repo-level `include` **replaces** `defaults.include`; a repo-level `exclude`
  is **added to** `defaults.exclude`.
- `prune_orphans` trashes docs that no longer match a configured repo. Skipped
  under `--only`, since a partial run does not represent the whole folder.
- The refresh token has no scheduled rotation. It survives password changes (no
  Gmail scope) and the 6-month idle limit (the schedule uses it daily). Re-run
  `cargo run -- auth` if it is ever revoked.
- `storageQuotaExceeded` means the credentials are a service account.
  `invalid_client` means the client ID/secret do not match. A repo collecting 0
  files usually means its globs or `.gitignore`.
