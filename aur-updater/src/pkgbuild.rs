//! Parsing and editing of PKGBUILD files.
//!
//! Handles pkgver/pkgrel bumps, `arch` extraction, and checksum splicing —
//! including multi-arch packages whose sums are split into per-architecture
//! lines (`sha256sums_x86_64` / `sha256sums_aarch64`).

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

/// Matches the `arch=(...)` value, tolerating surrounding whitespace.
fn arch_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)^arch=\(([^)]*)\)").unwrap())
}

/// Parse a bash single-line array like `'x86_64' 'aarch64'` into its elements.
/// Strips surrounding quotes and whitespace from each element.
fn parse_arches(raw: &str) -> Vec<String> {
    raw.split_whitespace()
        .map(|tok| tok.trim_matches(|c| c == '\'' || c == '"').to_string())
        .filter(|s| !s.is_empty())
        .collect()
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

/// Read the `arch=` array from a PKGBUILD. Empty if not found.
pub fn read_arches(pkgbuild: &Path) -> Result<Vec<String>> {
    let text = fs::read_to_string(pkgbuild)?;
    Ok(arch_re()
        .captures(&text)
        .map(|c| parse_arches(&c[1]))
        .unwrap_or_default())
}

/// Splice a single-line checksum array produced by `makepkg -g` (formatted as
/// `sha256sums=('…')`) into the PKGBUILD text, targeting a specific line name.
///
/// `target` is the full LHS array name:
///   - `"sha256sums"`      → the single-arch `sha256sums=(...)` line.
///   - `"sha256sums_x86_64"` / `"sha256sums_aarch64"` → the per-arch line.
///
/// If a line with that exact LHS exists, it is replaced in place; otherwise the
/// new line is inserted right after the last existing `sha256sums…=(…)` line (or
/// appended at the end if none). Returns the rewritten text.
pub fn splice_sums_block(text: &str, sums: &str, target: &str) -> Result<String> {
    // `sums` from `makepkg -g` is always `sha256sums=(...)`; reuse its RHS for the
    // target line so we don't reformat anything.
    let rhs = sums
        .strip_prefix("sha256sums=")
        .filter(|v| v.starts_with('(') && v.ends_with(')'))
        .ok_or_else(|| format!("malformed checksums block: {sums:?}"))?;
    let new_line = format!("{target}={rhs}");

    // Exact-match: replace the existing `target=(...)` line in place.
    // `[\s\S]*?` lets the match span a multi-line sums block.
    let exact = Regex::new(&format!(r"(?m)^{target}=\([\s\S]*?\)"))?;
    if exact.is_match(text) {
        return Ok(exact.replace(text, NoExpand(&new_line)).into_owned());
    }

    // No exact line yet: insert after the last `sha256sums…=(…)` line, else append.
    let any_sums = Regex::new(r"(?m)^sha256sums[_a-zA-Z0-9]*=\([\s\S]*?\)")?;
    if let Some(m) = any_sums.find_iter(text).last() {
        let mut out = String::with_capacity(text.len() + new_line.len() + 1);
        out.push_str(&text[..m.end()]);
        out.push('\n');
        out.push_str(&new_line);
        if let Some(rest) = text[m.end()..].strip_prefix('\n') {
            out.push_str(rest);
        } else {
            out.push_str(&text[m.end()..]);
        }
        Ok(out)
    } else {
        let mut out = text.to_string();
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&new_line);
        out.push('\n');
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_arches_handles_quotes_and_whitespace() {
        assert_eq!(
            parse_arches("'x86_64' 'aarch64'"),
            vec!["x86_64", "aarch64"]
        );
        assert_eq!(parse_arches("  'x86_64'  "), vec!["x86_64"]);
        assert!(parse_arches("").is_empty());
        // Double quotes tolerated too.
        assert_eq!(parse_arches("\"x86_64\""), vec!["x86_64"]);
    }

    const X64_SUM: &str =
        "sha256sums=('40bf72d8a086b4dddf1d9d0866001a6a0f547f84475c4d05ac58be3a177c0b3d')";
    const ARM_SUM: &str =
        "sha256sums=('bc6cbc9bc0c128dfe1a43367a804e464902bca62036fc0d67c433b3b5ef692d6')";

    fn pkbuild_text(sums_lines: &str) -> String {
        format!("pkgname=zcode-appimage\npkgver=3.2.2\narch=('x86_64' 'aarch64')\n{sums_lines}\n")
    }

    #[test]
    fn splice_replaces_single_arch_line_in_place() {
        let text = pkbuild_text("sha256sums=('OLD')");
        let out = splice_sums_block(&text, X64_SUM, "sha256sums").unwrap();
        assert!(out.contains(X64_SUM));
        assert!(!out.contains("'OLD'"));
    }

    #[test]
    fn splice_replaces_per_arch_line_in_place() {
        let text = pkbuild_text("sha256sums_x86_64=('OLD')\nsha256sums_aarch64=('OLDARM')");
        let out = splice_sums_block(&text, X64_SUM, "sha256sums_x86_64").unwrap();
        assert!(out.contains("sha256sums_x86_64=('40bf72d8"));
        // The other arch line must be untouched.
        assert!(out.contains("sha256sums_aarch64=('OLDARM')"));
    }

    #[test]
    fn splice_inserts_missing_per_arch_line_after_last_sums() {
        // Existing single-arch line, no per-arch line yet.
        let text = pkbuild_text("sha256sums=('OLD')");
        let out = splice_sums_block(&text, ARM_SUM, "sha256sums_aarch64").unwrap();
        assert!(out.contains("sha256sums=('OLD')"));
        assert!(out.contains("sha256sums_aarch64=('bc6cbc"));
        // Inserted after, on its own line, not mid-line.
        assert!(out.contains("sha256sums=('OLD')\nsha256sums_aarch64=('bc6cbc"));
    }

    #[test]
    fn splice_appends_when_no_sums_line_exists() {
        let text = "pkgname=zcode-appimage\npkgver=3.2.2\n".to_string();
        let out = splice_sums_block(&text, X64_SUM, "sha256sums_x86_64").unwrap();
        assert!(out.ends_with("sha256sums_x86_64=('40bf72d8a086b4dddf1d9d0866001a6a0f547f84475c4d05ac58be3a177c0b3d')\n"));
    }

    #[test]
    fn splice_rejects_malformed_sums() {
        let text = pkbuild_text("");
        let err = splice_sums_block(&text, "not a sums line", "sha256sums").unwrap_err();
        assert!(err.to_string().contains("malformed"));
    }

    #[test]
    fn splice_handles_multiline_source_block() {
        // Some PKGBUILDs span the sums across lines; the RHS extraction still works
        // because makepkg -g emits single-line, but the *target* may be multiline.
        let text = pkbuild_text("sha256sums_x86_64=(\n    'OLD'\n)");
        let out = splice_sums_block(&text, X64_SUM, "sha256sums_x86_64").unwrap();
        assert!(out.contains("sha256sums_x86_64=('40bf72d8"));
        assert!(!out.contains("'OLD'"));
    }
}
