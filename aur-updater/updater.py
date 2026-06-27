#!/usr/bin/env python3
"""AUR auto-updater orchestrator.

For each package declared in config.toml:
  1. run its version-detection script (outputs the latest upstream version),
  2. validate the version against a regex,
  3. compare against the current pkgver in the PKGBUILD,
  4. on a real bump: update pkgver/pkgrel + checksums, regenerate .SRCINFO,
     commit and push to the AUR remote (optionally to a mirror).
A single Telegram digest summarises every package.

Usage:
    updater.py [--dry-run] [--only <pkg>]
"""

from __future__ import annotations

import argparse
import re
import shutil
import subprocess
import sys
import tomllib
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path

import notify
import pkgbuild

REPO_ROOT = Path(__file__).resolve().parent.parent
CONFIG = REPO_ROOT / "aur-updater" / "config.toml"


@dataclass
class PkgConfig:
    name: str
    path: Path
    version_script: str
    version_regex: str
    reset_pkgrel: bool
    aur_remote: str
    push_mirror: bool


def load_config() -> dict:
    with CONFIG.open("rb") as f:
        return tomllib.load(f)


def parse_packages(cfg: dict) -> list[PkgConfig]:
    pkgs: list[PkgConfig] = []
    for name, p in cfg.get("packages", {}).items():
        path = REPO_ROOT / p["path"]
        pkgs.append(PkgConfig(
            name=name,
            path=path,
            version_script=p.get("version_script", "fetch-version.sh"),
            version_regex=p.get("version_regex", r"^\d+\.\d+\.\d+$"),
            reset_pkgrel=p.get("reset_pkgrel", True),
            aur_remote=p.get(
                "aur_remote",
                f"ssh://aur@aur.archlinux.org/{name}.git"),
            push_mirror=p.get("push_mirror", False),
        ))
    return pkgs


def run(cmd: list[str], cwd: Path, **kw) -> subprocess.CompletedProcess:
    """Run a command, raising on failure with context."""
    proc = subprocess.run(cmd, cwd=cwd, text=True, capture_output=True, **kw)
    if proc.returncode != 0:
        raise RuntimeError(
            f"{' '.join(cmd)} failed (exit {proc.returncode})\n"
            f"stdout:\n{proc.stdout}\nstderr:\n{proc.stderr}")
    return proc


def detect_version(pkg: PkgConfig) -> str:
    """Run the package's version script; return its trimmed stdout."""
    script = pkg.path / pkg.version_script
    if not script.exists():
        raise FileNotFoundError(f"{script}: introuvable")
    proc = subprocess.run(["bash", str(script)], cwd=pkg.path,
                          text=True, capture_output=True, timeout=300)
    if proc.returncode != 0:
        raise RuntimeError(
            f"{pkg.version_script} exited {proc.returncode}\n"
            f"stderr:\n{proc.stderr.strip()}")
    return proc.stdout.strip()


def git_clean_state(path: Path) -> bool:
    proc = subprocess.run(["git", "status", "--porcelain"],
                          cwd=path, text=True, capture_output=True)
    return proc.returncode == 0 and proc.stdout.strip() == ""


def git_reset(path: Path) -> None:
    subprocess.run(["git", "checkout", "--", "."], cwd=path,
                   capture_output=True)


def update_checksums(path: Path) -> None:
    """Refresh sha256sums via updpkgsums (pacman-contrib), fall back to makepkg -g."""
    if shutil.which("updpkgsums"):
        run(["updpkgsums"], cwd=path)
        return
    # Fallback: regenerate checksums block manually.
    sums = run(["makepkg", "-g"], cwd=path).stdout.strip()
    if not sums:
        raise RuntimeError("makepkg -g produced no checksums")
    text = (path / "PKGBUILD").read_text()
    text = re.sub(r"^sha256sums=.*?\)\s*$",
                  sums, text, count=1, flags=re.MULTILINE | re.DOTALL)
    (path / "PKGBUILD").write_text(text)


def regenerate_srcinfo(path: Path) -> None:
    proc = run(["makepkg", "--printsrcinfo"], cwd=path)
    (path / ".SRCINFO").write_text(proc.stdout)


def commit_and_push(pkg: PkgConfig, new_ver: str, dry_run: bool) -> None:
    msg = f"upgpkg: {pkg.name} {new_ver}-1"
    if dry_run:
        print(f"[dry-run] would commit: {msg} and push to {pkg.aur_remote}")
        return
    run(["git", "add", "-A"], cwd=pkg.path)
    run(["git", "commit", "-m", msg], cwd=pkg.path)
    run(["git", "push", pkg.aur_remote, "HEAD:master"], cwd=pkg.path)
    if pkg.push_mirror:
        run(["git", "push", "origin"], cwd=pkg.path)


def process(pkg: PkgConfig, dry_run: bool) -> notify.PkgResult:
    try:
        ver = detect_version(pkg)
        if not re.match(pkg.version_regex, ver):
            return notify.PkgResult(pkg.name, "BROKEN",
                                    f"sortie invalide: {ver!r}")
        info = pkgbuild.read_info(pkg.path / "PKGBUILD")
        cur = info.pkgver
        if ver == cur:
            return notify.PkgResult(pkg.name, "UP_TO_DATE", f"à jour ({cur})")
        # crude version comparison: equal-length numeric tuples.
        def vkey(s: str) -> tuple:
            parts = []
            for tok in s.split("."):
                num = re.sub(r"\D", "", tok)
                parts.append(int(num) if num else 0)
            return tuple(parts)
        if vkey(ver) < vkey(cur):
            return notify.PkgResult(pkg.name, "WARN",
                                    f"downgrade {cur} → {ver}")
        if dry_run:
            return notify.PkgResult(pkg.name, "UPDATED",
                                    f"[dry-run] {cur} → {ver} (non poussé)")
        pkgbuild.bump_pkgver(pkg.path / "PKGBUILD", ver, pkg.reset_pkgrel)
        update_checksums(pkg.path)
        regenerate_srcinfo(pkg.path)
        commit_and_push(pkg, ver, dry_run=False)
        return notify.PkgResult(pkg.name, "UPDATED", f"{cur} → {ver} (poussé)")
    except Exception as exc:
        # Roll back any local edits so a failed run leaves a clean tree.
        if not dry_run:
            git_reset(pkg.path)
        return notify.PkgResult(pkg.name, "FAILED", str(exc)[:200])


def main() -> int:
    ap = argparse.ArgumentParser(description="AUR auto-updater")
    ap.add_argument("--dry-run", action="store_true",
                    help="détecte et affiche, sans rien pousser")
    ap.add_argument("--only", metavar="PKG", help="ne traiter que ce paquet")
    args = ap.parse_args()

    cfg = load_config()
    pkgs = parse_packages(cfg)
    if args.only:
        pkgs = [p for p in pkgs if p.name == args.only]
        if not pkgs:
            print(f"paquet inconnu: {args.only}", file=sys.stderr)
            return 2

    tg = notify.TelegramNotifier()
    run_id = datetime.now().strftime("%Y-%m-%d %H:%M")
    results: list[notify.PkgResult] = []
    for pkg in pkgs:
        print(f"== {pkg.name}")
        res = process(pkg, args.dry_run)
        print(f"   {res.render()}")
        results.append(res)

    if cfg.get("telegram", {}).get("enabled", True):
        tg.send_digest(results, run_id)
    return 0


if __name__ == "__main__":
    sys.exit(main())
