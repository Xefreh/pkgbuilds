//! Parsing and editing of PKGBUILD files (pkgver / pkgrel).
//!
//! sha256sums are only ever touched as a whole via `updpkgsums`/`makepkg -g`
//! (see `update_checksums` in `main`), so there is no sums handling here.

use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use regex::{NoExpand, Regex};

use crate::Result;

fn pkgver_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)^pkgver=(.+)$").unwrap())
}

fn pkgrel_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)^pkgrel=(.+)$").unwrap())
}

pub struct PkgbuildInfo {
    pub pkgver: String,
    // Parsed to assert presence (a PKGBUILD missing pkgrel is an error); not read otherwise.
    #[allow(dead_code)]
    pub pkgrel: String,
}

/// Extract the current pkgver and pkgrel from a PKGBUILD.
pub fn read_info(pkgbuild: &Path) -> Result<PkgbuildInfo> {
    let text = fs::read_to_string(pkgbuild)?;
    let pkgver = pkgver_re().captures(&text).map(|c| c[1].trim().to_string());
    let pkgrel = pkgrel_re().captures(&text).map(|c| c[1].trim().to_string());
    match (pkgver, pkgrel) {
        (Some(pkgver), Some(pkgrel)) => Ok(PkgbuildInfo { pkgver, pkgrel }),
        _ => Err(format!("{}: pkgver/pkgrel not found", pkgbuild.display()).into()),
    }
}

/// Replace pkgver and (optionally) reset pkgrel to 1 in the PKGBUILD.
pub fn bump_pkgver(pkgbuild: &Path, new_ver: &str, reset_pkgrel: bool) -> Result<()> {
    let text = fs::read_to_string(pkgbuild)?;
    if !pkgver_re().is_match(&text) {
        return Err(format!("{}: pkgver line not found", pkgbuild.display()).into());
    }
    let repl = format!("pkgver={new_ver}");
    let mut updated = pkgver_re().replace_all(&text, NoExpand(&repl)).into_owned();
    if reset_pkgrel {
        if !pkgrel_re().is_match(&updated) {
            return Err(format!("{}: pkgrel line not found", pkgbuild.display()).into());
        }
        updated = pkgrel_re()
            .replace_all(&updated, NoExpand("pkgrel=1"))
            .into_owned();
    }
    fs::write(pkgbuild, updated)?;
    Ok(())
}
