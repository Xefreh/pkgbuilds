# AUR auto-updater

A daily orchestrator that detects new upstream versions for each AUR package,
updates the `PKGBUILD` + `.SRCINFO`, pushes to the AUR, and sends a single
Telegram digest (updated / up to date / broken upstream / failed).

A single self-contained Rust binary (`aur-updater`, edition 2024).

## Requirements

```sh
pacman -S rust base-devel pacman-contrib git curl
```

- `rust` — build the binary
- `base-devel` — `makepkg` (`--printsrcinfo`, `-g`)
- `pacman-contrib` — `updpkgsums` (otherwise falls back to `makepkg -g`)
- `git`, `curl` — AUR push and Telegram send

## Version script contract (`fetch-version.sh`)

Each package ships a script that prints **only the version** on stdout (e.g.
`3.1.8`) and exits 0; a non-zero exit means the upstream is broken (changed CDN,
etc.). Probing is up to you (curl, page parsing, JSON API…).

The output is validated against `version_regex`; a non-matching output marks the
package `BROKEN` — the guard against a CDN returning something other than a
version.

## Configuration (`config.toml`)

`[telegram]`: `enabled` toggles the digest. The bot token and chat id come from
the environment (`TG_BOT_TOKEN` / `TG_CHAT_ID`), never committed.

Each `[packages.<name>]` entry:

| key | default | meaning |
|-----|---------|---------|
| `path` | — | package directory (submodule), relative to the repo root |
| `version_script` | `fetch-version.sh` | version-detection script |
| `version_regex` | `^\d+\.\d+\.\d+$` | validates the detected version |
| `reset_pkgrel` | `true` | reset `pkgrel=1` on a `pkgver` bump |
| `aur_remote` | `ssh://aur@aur.archlinux.org/<name>.git` | AUR SSH remote |
| `push_mirror` | `false` | also `git push origin` (e.g. a GitHub mirror) |

## Setup

1. **Telegram secrets** — copy the template and fill it in:
   ```sh
   cp aur-updater/aur-updater.env.example ~/.config/aur-updater.env
   $EDITOR ~/.config/aur-updater.env   # TG_BOT_TOKEN, TG_CHAT_ID
   chmod 600 ~/.config/aur-updater.env
   ```
   Chat id: message the bot, then `curl "https://api.telegram.org/bot<TOKEN>/getUpdates"`.

2. **AUR SSH** — an SSH key registered for `aur@aur.archlinux.org` (see the AUR
   wiki). Each package's remote is `aur_remote` in `config.toml`.

3. **Build & enable the daily timer** — from `aur-updater/`, run `make install`.
   Adjust `WorkingDirectory` / `ExecStart` / `EnvironmentFile` in
   `aur-updater.service` if your checkout is not at
   `~/work/github.com/Xefreh/pkgbuilds`.

## Usage

Everything runs through the `Makefile` (from `aur-updater/`); run `make help` for
the full list.

| command | action |
|---------|--------|
| `make build` | compile the release binary |
| `make dry-run` | detect and print, push nothing |
| `make run` | real run (commit + AUR push + Telegram) |
| `make check` | fmt + clippy + build + tests |
| `make install` / `make uninstall` | manage the systemd `--user` timer |

To process a single package, pass `--only <pkg>` to the binary. Logs:
`journalctl --user -u aur-updater.service`.

## Notified states (single Telegram digest)

| State | Icon | Meaning |
|-------|------|---------|
| UPDATED | ✅ | pkgver bumped, checksums regenerated, pushed to AUR |
| UP_TO_DATE | ⏸ | detected version == current pkgver |
| BROKEN | ⚠️ | version script failed or output didn't match the regex |
| FAILED | ❌ | makepkg/updpkgsums/git push failure (auto rollback) |
| WARN | 🟡 | downgrade detected (new version < current) |

## Known limitation

If a CDN returns `200` for any version (false positive), the regex guard won't
catch it. The `updpkgsums` step actually downloads the asset, so a missing or
corrupt file fails the step (→ `FAILED`), which partly protects against this.
