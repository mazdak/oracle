# Oracle

This repository builds the `oracle` MCP server/CLI and provides helper scripts for local builds and easy installs.

## Easy install for developers

```bash
./scripts/install.sh
```

If you prefer a custom location, set `ORACLE_INSTALL_DIR` before running the script:

```bash
ORACLE_INSTALL_DIR="$HOME/.local/oracle-bin" ./scripts/install.sh
```

The script compiles `oracle`, copies `target/release/oracle` into the install directory, and prints the export command you can add to your shell init (`~/.zshrc`, `~/.bashrc`, etc.) so future shells pick up the binary.

## Install from GitHub releases

Download the latest pre-built binary from the GitHub releases page by streaming the release-aware installer script and letting it drop the `oracle` executable into `~/.local/bin` (or your custom install directory):

```bash
curl -sSL https://raw.githubusercontent.com/mazdak/oracle/main/scripts/install_release.sh | bash
```

The release installer:

- detects `uname -s` / `uname -m` to pick the correct target triple (`x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`).
- hits the GitHub Releases API for `mazdak/oracle`, finds the asset whose name contains that triple, and downloads it.
- extracts the archive (supports `.tar.gz`, `.tgz`, `.zip`, or a raw binary) and installs `oracle` into `ORACLE_INSTALL_DIR` (defaults to `~/.local/bin`).
- reminds you to add the install directory to `PATH` if it isn’t already listed.

You can customize behavior by setting these environment variables ahead of time:

| Variable | Description |
| --- | --- |
| `GITHUB_OWNER` | GitHub account or org owning the repo (default: `mazdak`). |
| `GITHUB_REPO` | Repository name (default: `oracle`). |
| `ORACLE_RELEASE_TAG` | Release tag to download instead of `latest`. |
| `ORACLE_TARGET_TRIPLE` | Override the detected target triple if you want to force a specific binary. |
| `ORACLE_ASSET_FILTER` | Substring used to match the release asset name (defaults to the target triple). |
| `ORACLE_INSTALL_DIR` | Directory for the final binary (default: `~/.local/bin`). |

For example, to install an older release into a custom directory:

```bash
GITHUB_OWNER=mazdak GITHUB_REPO=oracle ORACLE_RELEASE_TAG=v0.1.0 ORACLE_INSTALL_DIR="$HOME/.local/oracle-bin" \
  curl -sSL https://raw.githubusercontent.com/mazdak/oracle/main/scripts/install_release.sh | bash
```

The release installer requires `curl`, `python3`, and either `tar`/`unzip` (depending on how the release asset is packaged).

## Automatic releases from tags

Push a tag that follows `vMAJOR.MINOR.PATCH` (e.g. `git tag v0.5.0 && git push origin v0.5.0`) and the `release.yml` workflow will:

- run the same cross-platform build matrix as CI (macOS and Linux, x86_64 and arm64),(`.github/workflows/release.yml#L20`).
- create a GitHub Release for that tag using `actions/create-release` (`.github/workflows/release.yml#L9`)
- package each binary into `oracle-<tag>-<target>.tar.gz` and upload it as a release asset (`.github/workflows/release.yml#L42`).

The workflow sets `permissions: contents: write` so it can publish new releases; no manual release step is required once a tag is pushed.
