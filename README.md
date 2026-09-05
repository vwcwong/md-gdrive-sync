# md-gdrive-sync

Collects Markdown notes from a set of git repositories and publishes them to a
Google Drive folder as native Google Docs, so they can be used as NotebookLM
sources.

Each run clones the configured repositories, walks them for Markdown, and
renders one document per repository plus a combined document. Existing Docs are
**updated in place**, so their Drive IDs never change and NotebookLM sources
stay linked instead of needing to be re-added.

## How the output is structured

Every file becomes one heading carrying its full path, with the file's own
headings nested beneath it:

```
# Personal Notes
## Contents
## docs/api/auth.md      <- the file, path intact
### Auth                 <- the file's own H1
#### Tokens              <- the file's own H2
```

Markdown has only six heading levels, and a heading per directory level would
exhaust them before a deep file's own structure could be represented. Putting
the whole path in one heading keeps the directory structure lossless while
leaving every file the same budget for its own headings, however deep it sits.
Directory grouping still shows up in the file ordering and the Contents list.

Also applied per file: frontmatter is stripped (a `title:` is used as the file's
top heading), setext headings are rewritten so they demote correctly, code
blocks are passed through untouched so a `# comment` in a shell snippet stays a
comment, and images become `*[image: alt text]*` because Drive converts Markdown
images into base64 data URIs that render broken in a Doc.

## Setup

### 1. Development shell

Requires [Nix](https://nixos.org/download) with flakes and
[direnv](https://direnv.net).

```sh
direnv allow          # or: nix develop
cargo test
```

The Rust toolchain is pinned by `rust-toolchain.toml` and provided by the
flake — rustup is not needed.

### 2. Google Cloud project

1. Create a project at [console.cloud.google.com](https://console.cloud.google.com).
2. Enable the **Google Drive API**.
3. Configure the **OAuth consent screen**: User type *External*.
   - Add the scope `https://www.googleapis.com/auth/drive.file`.
   - **Set publishing status to "In production".** `drive.file` is a
     non-sensitive scope, so this needs no verification review — and it is what
     stops the refresh token expiring after 7 days, which is what happens in
     *Testing* status.
4. Create an **OAuth client ID** of type *Desktop app*. Note the client ID and
   client secret.

> Do not use a service account. Google's Drive API documentation states that
> "service accounts don't have storage quota and can't own any files", so a
> service account cannot write to a personal Drive folder even when that folder
> is shared with it — uploads fail with `storageQuotaExceeded`.

### 3. Mint a refresh token

```sh
export GOOGLE_CLIENT_ID=...apps.googleusercontent.com
export GOOGLE_CLIENT_SECRET=...
cargo run -- auth
```

This opens a loopback listener, prints a consent URL, and prints a refresh
token once you approve it. The token does not expire on a schedule; see
[Token lifetime](#token-lifetime).

### 4. Drive folder

Create the destination folder in Drive. Its ID is the last path segment of the
folder's URL:

```
https://drive.google.com/drive/folders/1AbCdEf...   <- this part
```

### 5. Configure

Edit `repos.yml`. Then check it without touching the network:

```sh
GDRIVE_FOLDER_ID=1AbCdEf... cargo run -- validate
```

### 6. Repository secrets

Set these under *Settings → Secrets and variables → Actions*:

| Secret | What it is |
|---|---|
| `GDRIVE_FOLDER_ID` | Destination folder ID from step 4 |
| `GOOGLE_CLIENT_ID` | OAuth client ID from step 2 |
| `GOOGLE_CLIENT_SECRET` | OAuth client secret from step 2 |
| `GOOGLE_REFRESH_TOKEN` | Refresh token from step 3 |
| `NOTES_REPO_TOKEN` | Fine-grained PAT with read-only **Contents** access to the private note repos. Omit if every repo is public |

### 7. First run

Run the workflow manually with **dry run** ticked first. That exercises
checkout, the Nix build, cloning and private-repo auth, and uploads the
rendered Markdown as a workflow artifact — all without touching Drive. Then run
it for real, then leave the daily schedule to it.

### 8. NotebookLM

Add the Docs from the Drive folder as sources. They only need adding once:
later runs overwrite the same files, so a source just needs its **Sync** button.

## Commands

```sh
cargo run -- validate                       # check repos.yml, no network
cargo run -- sync --dry-run                 # render to ./out, skip Drive
cargo run -- sync                           # render and publish
cargo run -- sync --only "Personal Notes"   # one repository (pruning is skipped)
cargo run -- auth                           # mint a refresh token
cargo run -- -v sync                        # debug logging; RUST_LOG overrides
```

`sync` writes rendered Markdown to `./out` on every run, not only dry ones, so
there is always a local artifact to diff.

## Configuration

See the comments in `repos.yml`. Two merge rules are worth knowing:

- A repo-level `include` **replaces** `defaults.include` — a repo that opts into
  a narrower set of files means it.
- A repo-level `exclude` is **added to** `defaults.exclude` — per-repo excludes
  are almost always extra rules rather than a replacement.

Files ignored by a repository's own `.gitignore`, and dotfiles, are always
skipped.

`prune_orphans: true` trashes Docs this tool created that no longer match any
configured repo. They go to the Drive bin, not straight to deletion. Pruning is
skipped when `--only` is used, since a partial run does not represent the whole
folder.

## Token lifetime

The refresh token has no scheduled rotation. Google
[documents](https://developers.google.com/identity/protocols/oauth2#expiration)
every way one can stop working; for this setup:

| Condition | Applies |
|---|---|
| App left in *Testing* publishing status → 7 days | No, if step 2 was completed |
| Unused for 6 months | No — the schedule uses it daily |
| Password change *with Gmail scopes* | No — this requests Drive only |
| Access revoked in Google Account settings | Only if you do it |
| More than 100 live tokens for one account and client | Only after re-running `auth` 100+ times |

If it ever does need replacing, re-run `cargo run -- auth` and update the
secret.

## Troubleshooting

| Symptom | Cause |
|---|---|
| `storageQuotaExceeded` | The credentials belong to a service account. Use a user account (step 2). |
| `File not found` on the folder | Wrong `folder_id`, or the folder belongs to a different account than the token. |
| `invalid_client` | Client ID/secret do not match the OAuth client. |
| Refresh token missing after `auth` | The account already granted this client. Revoke the app in Google Account settings and re-run. |
| A repo collects 0 files | Check its `include`/`exclude` globs, and whether `.gitignore` covers the notes. |
| Private repo clone fails | `NOTES_REPO_TOKEN` missing, expired, or lacking Contents access to that repo. |
