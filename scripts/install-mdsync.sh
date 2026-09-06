#!/usr/bin/env bash
# Resolve a mdsync binary for the composite action.
#
# Prints `method=release` or `method=source` to $GITHUB_OUTPUT. A release
# install also puts the binary on PATH; a source result means the caller
# should `cargo build --release` from $GITHUB_ACTION_PATH.
set -euo pipefail

bin_dir="${RUNNER_TEMP:?}/mdsync-bin"
mkdir -p "$bin_dir"

repo="${GITHUB_ACTION_REPOSITORY:-${GITHUB_REPOSITORY:?}}"
ref="${GITHUB_ACTION_REF:-}"
version="${MDSYNC_VERSION:-}"

asset_name() {
  case "${RUNNER_OS:-}-$(uname -m)" in
    Linux-x86_64) echo "mdsync-x86_64-unknown-linux-gnu" ;;
    *) echo "" ;;
  esac
}

try_download() {
  local tag="$1"
  local asset
  asset="$(asset_name)"
  if [[ -z "$asset" ]]; then
    echo "No prebuilt binary for ${RUNNER_OS:-unknown} $(uname -m)."
    return 1
  fi

  local url="https://github.com/${repo}/releases/download/${tag}/${asset}"
  echo "Downloading ${url}"

  local curl_args=(-fsSL)
  if [[ -n "${GITHUB_TOKEN:-}" ]]; then
    curl_args+=(-H "Authorization: Bearer ${GITHUB_TOKEN}")
  fi

  if curl "${curl_args[@]}" -o "${bin_dir}/mdsync" "$url"; then
    chmod +x "${bin_dir}/mdsync"
    echo "${bin_dir}" >> "${GITHUB_PATH:?}"
    echo "method=release" >> "${GITHUB_OUTPUT:?}"
    "${bin_dir}/mdsync" --version
    return 0
  fi
  return 1
}

want_source() {
  echo "method=source" >> "${GITHUB_OUTPUT:?}"
}

if [[ "$version" == "source" ]]; then
  want_source
  exit 0
fi

if [[ -n "$version" ]]; then
  if try_download "$version"; then
    exit 0
  fi
  echo "error: could not download mdsync ${version} for this runner" >&2
  exit 1
fi

if [[ "$ref" == v* ]]; then
  if try_download "$ref"; then
    exit 0
  fi
  echo "No release asset for ${ref}; building this checkout from source."
fi

want_source
