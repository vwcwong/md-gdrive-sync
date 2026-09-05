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

**First run** — trigger the workflow by hand with *dry run* ticked. The
rendered Markdown is uploaded as an artifact and Drive is left untouched.
When that looks right, run it for real and let the daily schedule take over.

## Commands

```sh
cargo run -- validate                       # check repos.yml, no network
cargo run -- sync --dry-run                 # render to ./out, skip Drive
cargo run -- sync                           # render and publish
cargo run -- sync --only "Personal Notes"   # one repo (skips pruning)
cargo run -- auth                           # get a refresh token
```

Rendered Markdown is always written to `./out`, even when Drive is skipped.

