# md-gdrive-sync

Keep NotebookLM sources current by syncing Markdown from Git
repositories into Google Drive.

One document per repository, plus an optional combined document of
everything.

## What you get

Each Markdown file becomes a section titled with its path. Headings inside
the file nest underneath:

```
## docs/api/auth.md
### Auth
#### Tokens
```

## Use it in your repository

The usual setup is a `repos.yml` plus a GitHub Actions workflow that runs on
a schedule.

### 1. List the repositories to publish

Create a `repos.yml` in the repository that will run the workflow:

```yaml
drive:
  folder_id: ${GDRIVE_FOLDER_ID}
  combined_doc_name: "Notes — All Repos"
  emit_combined: true
  emit_per_repo: true
  prune_orphans: true

repos:
  - url: https://github.com/owner/notes
    name: "Personal Notes"
  - url: https://github.com/owner/work-notes
    name: "Work Notes"
    private: true
```

`${GDRIVE_FOLDER_ID}` is filled from the environment. The folder ID is the
last segment of the Drive folder's URL.

`name` is the Google Doc title. If you omit it, the last path segment of
`url` is used. Names must be unique. Private repositories need
`private: true` and a `NOTES_REPO_TOKEN` secret that can clone them (HTTPS
only; SSH remotes are not supported).

A fuller example, including include/exclude globs, branches, and local
`file://` paths, is in [`repo.examples.yaml`](repo.examples.yaml).

### 2. Create a Google Cloud OAuth client

Create a Google Cloud project, enable the Drive API, then:

1. Publish the OAuth consent screen. If you leave it in *Testing*, the
   refresh token expires every 7 days.
2. Create an OAuth client ID of type **Desktop app**. Do not use a service
   account.

The app only requests the `drive.file` scope: it can create and update files
it owns, and cannot see the rest of your Drive.

### 3. Mint a refresh token

This step is interactive and only needs to happen once. Install the CLI and
run:

```sh
cargo install --git https://github.com/vwcwong/md-gdrive-sync
export GOOGLE_CLIENT_ID=... GOOGLE_CLIENT_SECRET=...
mdsync auth
```

Store the printed token as `GOOGLE_REFRESH_TOKEN` on the repository that
will run the workflow. `auth` is not part of the Action.

### 4. Add repository secrets

Under *Settings → Secrets and variables → Actions*:

| Secret | Purpose |
| --- | --- |
| `GDRIVE_FOLDER_ID` | Destination Drive folder |
| `GOOGLE_CLIENT_ID` | OAuth client ID from the Desktop app |
| `GOOGLE_CLIENT_SECRET` | Matching client secret |
| `GOOGLE_REFRESH_TOKEN` | From `mdsync auth` |
| `NOTES_REPO_TOKEN` | GitHub token that can clone private repos; omit if every repo is public |

### 5. Add a workflow

Pin a release tag so the runner downloads the Linux binary instead of
compiling:

```yaml
name: sync

on:
  schedule:
    - cron: "17 4 * * *"
  workflow_dispatch:

jobs:
  sync:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - uses: vwcwong/md-gdrive-sync@v1.0.0
        with:
          config: repos.yml
        env:
          GDRIVE_FOLDER_ID: ${{ secrets.GDRIVE_FOLDER_ID }}
          GOOGLE_CLIENT_ID: ${{ secrets.GOOGLE_CLIENT_ID }}
          GOOGLE_CLIENT_SECRET: ${{ secrets.GOOGLE_CLIENT_SECRET }}
          GOOGLE_REFRESH_TOKEN: ${{ secrets.GOOGLE_REFRESH_TOKEN }}
          NOTES_REPO_TOKEN: ${{ secrets.NOTES_REPO_TOKEN }}
```

Optional inputs: `dry-run` (`true` to render without uploading), `out`
(directory for the rendered Markdown, default `out`), `only` (comma-separated
repo names; skips pruning), and `version` (override which binary to download).

Do not upload the `out` folder as a workflow artifact if any source repo is
private: that would publish its contents to anyone who can see the run.

## Preview locally

Before the first real upload, render without touching Drive:

```sh
export GDRIVE_FOLDER_ID=...
mdsync validate
mdsync sync --dry-run
```

Inspect `./out`. When it looks right, run `mdsync sync` or trigger the
workflow.

```sh
mdsync validate                       # check repos.yml, no network
mdsync sync --dry-run                 # render to ./out, skip Drive
mdsync sync                           # render and publish
mdsync sync --only "Personal Notes"   # one repo (skips pruning)
mdsync auth                           # get a refresh token
```

Rendered Markdown is always written to `./out`, even when Drive is skipped.
