"""Parsing and editing of PKGBUILD files (pkgver / pkgrel / sha256sums)."""

from __future__ import annotations

import re
from dataclasses import dataclass
from pathlib import Path

_PKGVER_RE = re.compile(r"^pkgver=(.+)$", re.MULTILINE)
_PKGREL_RE = re.compile(r"^pkgrel=(.+)$", re.MULTILINE)
# sha256sums may be a single quoted string or an array entry; we only touch the
# sums array as a whole via `updpkgsums`/`makepkg -g`, so no manual regex here.


@dataclass
class PkgbuildInfo:
    pkgver: str
    pkgrel: str


def read_info(pkgbuild: Path) -> PkgbuildInfo:
    """Extract the current pkgver and pkgrel from a PKGBUILD."""
    text = pkgbuild.read_text()
    m_ver = _PKGVER_RE.search(text)
    m_rel = _PKGREL_RE.search(text)
    if not m_ver or not m_rel:
        raise ValueError(f"{pkgbuild}: pkgver/pkgrel introuvables")
    return PkgbuildInfo(pkgver=m_ver.group(1).strip(),
                        pkgrel=m_rel.group(1).strip())


def bump_pkgver(pkgbuild: Path, new_ver: str, reset_pkgrel: bool) -> None:
    """Replace pkgver and (optionally) reset pkgrel to 1 in the PKGBUILD."""
    text = pkgbuild.read_text()
    if not _PKGVER_RE.search(text):
        raise ValueError(f"{pkgbuild}: ligne pkgver introuvable")
    text = _PKGVER_RE.sub(f"pkgver={new_ver}", text)
    if reset_pkgrel:
        if not _PKGREL_RE.search(text):
            raise ValueError(f"{pkgbuild}: ligne pkgrel introuvable")
        text = _PKGREL_RE.sub("pkgrel=1", text)
    pkgbuild.write_text(text)
