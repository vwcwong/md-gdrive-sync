# md-gdrive-sync

If your notes live in Git, one convenient way to use them in NotebookLM is as
Google Docs in Drive. This publishes Markdown from a list of repos into a
folder there — one document per repo, plus a combined document of everything.

Each run updates the existing files rather than creating new ones, so sources
you have already attached keep working.

## Output

Each file becomes a section titled with its full path. Headings inside the
file nest underneath:

```
## docs/api/auth.md
### Auth
#### Tokens
```

Frontmatter is stripped; a `title:` field is used as the heading instead.
Code blocks are unchanged. Images become a text placeholder. Dotfiles and
anything matched by `.gitignore` are skipped.

## Setup

```sh
direnv allow        # or: nix develop
cargo test
```

**Google Cloud** — create a project, enable the Drive API, then:

1. Publish the OAuth consent screen. If you leave it in *Testing*, the
   refresh token expires every 7 days.
2. Create an OAuth client ID of type **Desktop app**. Do not use a service
   account.

**Refresh token**

```sh
export GOOGLE_CLIENT_ID=... GOOGLE_CLIENT_SECRET=...
cargo run -- auth
```

**Configure** — edit `repos.yml`, then
`GDRIVE_FOLDER_ID=<id> cargo run -- validate`. The folder ID is the last segment
of the Drive folder's URL.

**Secrets** — under *Settings → Secrets and variables → Actions*:

- `GDRIVE_FOLDER_ID` — destination Drive folder
- `GOOGLE_CLIENT_ID` — OAuth client ID from the Desktop app
- `GOOGLE_CLIENT_SECRET` — matching client secret
- `GOOGLE_REFRESH_TOKEN` — from `cargo run -- auth`
- `NOTES_REPO_TOKEN` — a GitHub token that can clone private repos; skip it
  if every repo is public

**First run** — `cargo run -- sync --dry-run` and inspect `./out`. CI does
not upload that folder: it would publish private-repo contents as a public
artifact. When the local render looks right, trigger the workflow for real
and let the daily schedule take over.

## GitHub Action

Other repositories can run the same publish path with a step. Pin a release
tag so the runner downloads the Linux binary instead of compiling:

```yaml
- uses: actions/checkout@v5
- uses: vwcwong/md-gdrive-sync@v0.1.0
  with:
    config: repos.yml
  env:
    GDRIVE_FOLDER_ID: ${{ secrets.GDRIVE_FOLDER_ID }}
    GOOGLE_CLIENT_ID: ${{ secrets.GOOGLE_CLIENT_ID }}
    GOOGLE_CLIENT_SECRET: ${{ secrets.GOOGLE_CLIENT_SECRET }}
    GOOGLE_REFRESH_TOKEN: ${{ secrets.GOOGLE_REFRESH_TOKEN }}
    NOTES_REPO_TOKEN: ${{ secrets.NOTES_REPO_TOKEN }}
```

`auth` is not part of the action: mint the refresh token locally, then store
it as a secret on the calling repository. Inputs are `config`, `dry-run`,
`out`, `only`, and `version`. Secrets stay in `env` so they are not logged
as inputs.

This repository's `sync` workflow uses `uses: ./` with `version: source` so
a scheduled run always matches the commit it is building. Push a `v*` tag to
cut a GitHub Release and attach the Linux x64 binary other repos download.

## Commands

```sh
cargo run -- validate                       # check repos.yml, no network
cargo run -- sync --dry-run                 # render to ./out, skip Drive
cargo run -- sync                           # render and publish
cargo run -- sync --only "Personal Notes"   # one repo (skips pruning)
cargo run -- auth                           # get a refresh token
```

Rendered Markdown is always written to `./out`, even when Drive is skipped.

