# AUR auto-updater

A daily orchestrator that detects new upstream versions for each AUR package,
updates the `PKGBUILD` + `.SRCINFO`, pushes to the AUR, and sends a Telegram
summary (updated / already up to date / broken upstream / failed).

## Stack
- Rust (edition 2024) — `aur-updater` binary: orchestrator and PKGBUILD parsing
- Per-package bash scripts for version detection (e.g. `fetch-version.sh`)
- systemd timer — daily trigger at 20:00
- TOML — configuration manifest

System tools required at runtime: `git`, `makepkg` (base-devel), `curl`
(Telegram send) and, preferably, `updpkgsums` (pacman-contrib).

## Layout

```
pkgbuilds/
├── aur-updater/
│   ├── Cargo.toml            # Rust manifest (edition 2024)
│   ├── Cargo.lock
│   ├── src/
│   │   ├── main.rs           # orchestrator (CLI: --dry-run, --only <pkg>)
│   │   ├── pkgbuild.rs       # read/edit pkgver, pkgrel
│   │   └── notify.rs         # Telegram notification (single digest)
│   ├── config.toml           # package manifest
│   ├── aur-updater.service   # oneshot unit (User, EnvironmentFile)
│   ├── aur-updater.timer     # OnCalendar 20:00
│   ├── aur-updater.env.example # Telegram secrets template
│   └── README.md
└── <pkg>/                    # each package (submodule)
    ├── PKGBUILD
    ├── .SRCINFO
    └── fetch-version.sh      # detection script (outputs the version on stdout)
```

## Version script contract (`fetch-version.sh`)

- Output **only the version on stdout** (e.g. `3.1.8`).
- `exit 0` on success; non-zero `exit` if the upstream is broken (changed CDN, etc.).
- The user is free to implement the probing however they like (curl, gallop
  search, page parsing, JSON API…).

The detected version is validated by `version_regex` (config); any non-matching
output marks the package `BROKEN` (⚠️) — this is the guard against a CDN that
would return something other than a version.

## Installation

### 1. Telegram secrets

```sh
cp aur-updater/aur-updater.env.example ~/.config/aur-updater.env
$EDITOR ~/.config/aur-updater.env   # fill in TG_BOT_TOKEN and TG_CHAT_ID
chmod 600 ~/.config/aur-updater.env
```

To retrieve the chat id: send a message to the bot, then
`curl "https://api.telegram.org/bot<TOKEN>/getUpdates"`.

### 2. SSH for the AUR

SSH key configured for `aur@aur.archlinux.org` (see the AUR wiki). The SSH
remote of each package is `aur_remote` in `config.toml`.

### 3. Arch dependencies

```sh
pacman -S rust base-devel pacman-contrib git curl
# rust          : compile the binary (edition 2024)
# base-devel    : makepkg (--printsrcinfo, -g)
# pacman-contrib: updpkgsums (otherwise fallback to makepkg -g)
# git, curl     : AUR push and Telegram digest send
```

### 4. Compile the binary

```sh
cargo build --release --manifest-path aur-updater/Cargo.toml
# produces aur-updater/target/release/aur-updater
```

### 5. Install the systemd timer (--user)

```sh
mkdir -p ~/.config/systemd/user
ln -sf "$PWD/aur-updater/aur-updater.service" ~/.config/systemd/user/
ln -sf "$PWD/aur-updater/aur-updater.timer"   ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now aur-updater.timer
```

Adjust `WorkingDirectory` / `ExecStart` / `EnvironmentFile` in the `.service`
if the repository is not located at `~/work/github.com/Xefreh/pkgbuilds`.

## Usage

Run from the repository: the binary locates `aur-updater/config.toml` by
walking up the tree from the current directory.

```sh
BIN=aur-updater/target/release/aur-updater

# Dry run: detect and display, push nothing.
"$BIN" --dry-run

# Process only one package.
"$BIN" --only zcode-appimage

# Real run (commit + AUR push + Telegram).
"$BIN"

# systemd logs.
journalctl --user -u aur-updater.service
```

## Notified states (single Telegram digest)

| State       | Icon | Meaning                                          |
|-------------|------|--------------------------------------------------|
| UPDATED     | ✅   | pkgver bumped, checksums regenerated, pushed AUR |
| UP_TO_DATE  | ⏸   | detected version == current pkgver               |
| BROKEN      | ⚠️   | version script failed or non-regex output        |
| FAILED      | ❌   | makepkg/updpkgsums/git push failure (auto rollback) |
| WARN        | 🟡   | downgrade detected (new version < current)       |

## Known limitation

If a CDN returns `200` for any version (false positive), the regex guard does
not detect it. The `updpkgsums` step actually downloads the asset; a missing or
corrupted file causes the step to fail (→ `FAILED`), which partly protects
against this case. An optional asset sanity check (size/magic) is planned as a
future extension.
