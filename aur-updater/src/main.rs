//! AUR auto-updater orchestrator.
//!
//! For each package declared in config.toml:
//!   1. run its version-detection script (outputs the latest upstream version),
//!   2. validate the version against a regex,
//!   3. compare against the current pkgver in the PKGBUILD,
//!   4. on a real bump: update pkgver/pkgrel + checksums, regenerate .SRCINFO,
//!      commit and push to the AUR remote (optionally to a mirror).
//!
//! A single Telegram digest summarises every package.
//!
//! Usage: aur-updater [--dry-run] [--only <pkg>]

mod notify;
mod pkgbuild;

use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use regex::{NoExpand, Regex};
use serde::Deserialize;

use notify::{PkgResult, TelegramNotifier};

/// Shared error type: any error boxed, message available via `Display`.
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

const VERSION_SCRIPT_DEFAULT: &str = "fetch-version.sh";
const VERSION_REGEX_DEFAULT: &str = r"^\d+\.\d+\.\d+$";
const DETECT_TIMEOUT: Duration = Duration::from_secs(300);

const HELP: &str = "\
aur-updater — AUR auto-updater

USAGE:
    aur-updater [--dry-run] [--only <PKG>]

OPTIONS:
    --dry-run       detect and print, without pushing anything
    --only <PKG>    process only this package
    -h, --help      show this help
";

/// A resolved package, ready to process.
struct Pkg {
    name: String,
    path: PathBuf,
    version_script: String,
    version_regex: String,
    reset_pkgrel: bool,
    aur_remote: String,
    push_mirror: bool,
}

/// Raw `config.toml` schema.
#[derive(Deserialize)]
struct Config {
    #[serde(default)]
    telegram: Telegram,
    #[serde(default)]
    packages: IndexMap<String, RawPkg>,
}

#[derive(Deserialize)]
struct Telegram {
    #[serde(default = "default_true")]
    enabled: bool,
}

impl Default for Telegram {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Deserialize)]
struct RawPkg {
    path: String,
    version_script: Option<String>,
    version_regex: Option<String>,
    reset_pkgrel: Option<bool>,
    aur_remote: Option<String>,
    push_mirror: Option<bool>,
}

fn default_true() -> bool {
    true
}

/// Parsed command-line arguments.
struct Args {
    dry_run: bool,
    only: Option<String>,
}

impl Args {
    fn parse(mut it: impl Iterator<Item = String>) -> Result<Self> {
        let mut dry_run = false;
        let mut only = None;
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--dry-run" => dry_run = true,
                "--only" => {
                    only = Some(it.next().ok_or("--only requires an argument <PKG>")?);
                }
                rest if rest.starts_with("--only=") => {
                    only = Some(rest["--only=".len()..].to_string());
                }
                "-h" | "--help" => {
                    print!("{HELP}");
                    std::process::exit(0);
                }
                other => return Err(format!("unknown argument: {other}").into()),
            }
        }
        Ok(Self { dry_run, only })
    }
}

/// Locate the repo root by walking up from the cwd until `aur-updater/config.toml`
/// is found. The systemd service sets WorkingDirectory to the repo root, so in
/// production the cwd already matches.
fn find_repo_root() -> Result<PathBuf> {
    let start = env::current_dir()?;
    let mut dir = start.as_path();
    loop {
        if dir.join("aur-updater/config.toml").is_file() {
            return Ok(dir.to_path_buf());
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => {
                return Err(format!(
                    "config.toml not found (looked for aur-updater/config.toml \
                     walking up from {})",
                    start.display()
                )
                .into());
            }
        }
    }
}

fn load_config(path: &Path) -> Result<Config> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(toml::from_str(&text)?)
}

/// Resolve raw config entries into `Pkg`, applying the same defaults as before.
fn resolve_packages(cfg: &Config, repo_root: &Path) -> Vec<Pkg> {
    cfg.packages
        .iter()
        .map(|(name, p)| Pkg {
            name: name.clone(),
            path: repo_root.join(&p.path),
            version_script: p
                .version_script
                .clone()
                .unwrap_or_else(|| VERSION_SCRIPT_DEFAULT.to_string()),
            version_regex: p
                .version_regex
                .clone()
                .unwrap_or_else(|| VERSION_REGEX_DEFAULT.to_string()),
            reset_pkgrel: p.reset_pkgrel.unwrap_or(true),
            aur_remote: p
                .aur_remote
                .clone()
                .unwrap_or_else(|| format!("ssh://aur@aur.archlinux.org/{name}.git")),
            push_mirror: p.push_mirror.unwrap_or(false),
        })
        .collect()
}

/// Run a command, returning an error with context on failure.
fn run(cmd: &[&str], cwd: &Path) -> Result<std::process::Output> {
    run_with_env(cmd, cwd, &[])
}

/// Like `run`, but additionally sets `envs` (name, value) pairs in the child.
fn run_with_env(cmd: &[&str], cwd: &Path, envs: &[(&str, &str)]) -> Result<std::process::Output> {
    let out = Command::new(cmd[0])
        .args(&cmd[1..])
        .current_dir(cwd)
        .envs(envs.iter().copied())
        .output()
        .map_err(|e| format!("{}: {e}", cmd.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "{} failed (exit {})\nstdout:\n{}\nstderr:\n{}",
            cmd.join(" "),
            exit_code(&out.status),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        )
        .into());
    }
    Ok(out)
}

fn exit_code(status: &std::process::ExitStatus) -> String {
    status
        .code()
        .map_or_else(|| "?".to_string(), |c| c.to_string())
}

/// `shutil.which`: is `cmd` an executable file on PATH?
fn which(cmd: &str) -> bool {
    env::var_os("PATH")
        .is_some_and(|paths| env::split_paths(&paths).any(|dir| is_executable(&dir.join(cmd))))
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// Run the package's version script; return its trimmed stdout.
fn detect_version(pkg: &Pkg) -> Result<String> {
    let script = pkg.path.join(&pkg.version_script);
    if !script.exists() {
        return Err(format!("{}: not found", script.display()).into());
    }

    let mut child = Command::new("bash")
        .arg(&script)
        .current_dir(&pkg.path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("bash {}: {e}", script.display()))?;

    // Drain stdout/stderr on threads so a chatty script can't deadlock on a full pipe.
    let mut out_pipe = child.stdout.take().expect("piped stdout");
    let mut err_pipe = child.stderr.take().expect("piped stderr");
    let out_reader = thread::spawn(move || {
        let mut s = String::new();
        let _ = out_pipe.read_to_string(&mut s);
        s
    });
    let err_reader = thread::spawn(move || {
        let mut s = String::new();
        let _ = err_pipe.read_to_string(&mut s);
        s
    });

    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() >= DETECT_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "bash {} timed out after {} seconds",
                script.display(),
                DETECT_TIMEOUT.as_secs()
            )
            .into());
        }
        thread::sleep(Duration::from_millis(100));
    };

    let stdout = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();

    if !status.success() {
        return Err(format!(
            "{} exited {}\nstderr:\n{}",
            pkg.version_script,
            exit_code(&status),
            stderr.trim()
        )
        .into());
    }
    Ok(stdout.trim().to_string())
}

/// Roll back local edits so a failed run leaves a clean tree.
fn git_reset(path: &Path) {
    let _ = Command::new("git")
        .args(["checkout", "--", "."])
        .current_dir(path)
        .output();
}

/// Refresh sha256sums. Single-arch packages delegate to `updpkgsums` (pacman-contrib)
/// with a `makepkg -g` fallback. Multi-arch packages (e.g. `x86_64` + `aarch64`)
/// need per-arch sums: `updpkgsums` can't produce those on a single host, so we run
/// `makepkg -g` once per arch with `CARCH` overridden and splice each result into its
/// `sha256sums_<arch>` line.
fn update_checksums(path: &Path) -> Result<()> {
    let arches = pkgbuild::read_arches(&path.join("PKGBUILD"))?;

    // Single-arch (or no arch= at all): use the fast path.
    if arches.len() <= 1 {
        if which("updpkgsums") {
            run(&["updpkgsums"], path)?;
            return Ok(());
        }
        let out = run(&["makepkg", "-g"], path)?;
        let sums = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if sums.is_empty() {
            return Err("makepkg -g produced no checksums".into());
        }
        let pkgbuild = path.join("PKGBUILD");
        let text = fs::read_to_string(&pkgbuild)?;
        let updated = pkgbuild::splice_sums_block(&text, &sums, "sha256sums")?;
        fs::write(&pkgbuild, updated)?;
        return Ok(());
    }

    // Multi-arch: compute one checksums block per arch, splice each into its own line.
    let pkgbuild = path.join("PKGBUILD");
    let mut text = fs::read_to_string(&pkgbuild)?;
    for arch in &arches {
        let out = run_with_env(&["makepkg", "-g"], path, &[("CARCH", arch.as_str())])?;
        let sums = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if sums.is_empty() {
            return Err(format!("makepkg -g (CARCH={arch}) produced no checksums").into());
        }
        let target = format!("sha256sums_{arch}");
        text = pkgbuild::splice_sums_block(&text, &sums, &target)?;
    }
    // Drop any now-stale single-arch `sha256sums=(...)` line so makepkg doesn't
    // error out on conflicting generic + per-arch sums.
    text = strip_generic_sums_line(&text)?;
    fs::write(&pkgbuild, text)?;
    Ok(())
}

/// Remove a generic `sha256sums=(...)` line when per-arch sums are present.
/// Harmless if there's nothing to remove.
fn strip_generic_sums_line(text: &str) -> Result<String> {
    let re = regex::Regex::new(r"(?m)^sha256sums=\(.*\)\n?")?;
    Ok(re.replace(text, NoExpand("")).into_owned())
}

/// Regenerate .SRCINFO from the current PKGBUILD.
fn regenerate_srcinfo(path: &Path) -> Result<()> {
    let out = run(&["makepkg", "--printsrcinfo"], path)?;
    fs::write(path.join(".SRCINFO"), &out.stdout)?;
    Ok(())
}

fn commit_and_push(pkg: &Pkg, new_ver: &str) -> Result<()> {
    let msg = format!("upgpkg: {} {new_ver}-1", pkg.name);
    run(&["git", "add", "PKGBUILD", ".SRCINFO"], &pkg.path)?;
    run(&["git", "commit", "-m", msg.as_str()], &pkg.path)?;
    run(
        &["git", "push", pkg.aur_remote.as_str(), "HEAD:master"],
        &pkg.path,
    )?;
    if pkg.push_mirror {
        run(&["git", "push", "origin"], &pkg.path)?;
    }
    Ok(())
}

/// Crude version comparison key: a tuple of integers, digits extracted per token.
fn vkey(s: &str) -> Vec<u64> {
    s.split('.')
        .map(|tok| {
            let digits: String = tok.chars().filter(char::is_ascii_digit).collect();
            digits.parse().unwrap_or(0)
        })
        .collect()
}

/// Reproduce `re.match`: the match must start at index 0.
fn matches_at_start(re: &Regex, s: &str) -> bool {
    re.find(s).is_some_and(|m| m.start() == 0)
}

/// Process a package; never propagates an error (failures become a FAILED result).
fn process(pkg: &Pkg, dry_run: bool) -> PkgResult {
    match process_inner(pkg, dry_run) {
        Ok(res) => res,
        Err(err) => {
            if !dry_run {
                git_reset(&pkg.path);
            }
            let detail: String = err.to_string().chars().take(200).collect();
            PkgResult::new(&pkg.name, "FAILED", &detail)
        }
    }
}

fn process_inner(pkg: &Pkg, dry_run: bool) -> Result<PkgResult> {
    let ver = detect_version(pkg)?;
    let re = Regex::new(&pkg.version_regex)
        .map_err(|e| format!("invalid version_regex {:?}: {e}", pkg.version_regex))?;
    if !matches_at_start(&re, &ver) {
        return Ok(PkgResult::new(
            &pkg.name,
            "BROKEN",
            &format!("invalid output: {ver:?}"),
        ));
    }
    let info = pkgbuild::read_info(&pkg.path.join("PKGBUILD"))?;
    let cur = info.pkgver;
    if ver == cur {
        return Ok(PkgResult::new(&pkg.name, "UP_TO_DATE", &cur));
    }
    if vkey(&ver) < vkey(&cur) {
        return Ok(PkgResult::new(
            &pkg.name,
            "WARN",
            &format!("downgrade {cur} → {ver}"),
        ));
    }
    if dry_run {
        return Ok(PkgResult::new(
            &pkg.name,
            "UPDATED",
            &format!("[dry-run] {cur} → {ver} (not pushed)"),
        ));
    }
    pkgbuild::bump_pkgver(&pkg.path.join("PKGBUILD"), &ver, pkg.reset_pkgrel)?;
    update_checksums(&pkg.path)?;
    regenerate_srcinfo(&pkg.path)?;
    commit_and_push(pkg, &ver)?;
    Ok(PkgResult::new(
        &pkg.name,
        "UPDATED",
        &format!("{cur} → {ver} (pushed)"),
    ))
}

fn run_main() -> Result<ExitCode> {
    let args = Args::parse(env::args().skip(1))?;

    let repo_root = find_repo_root()?;
    let cfg = load_config(&repo_root.join("aur-updater/config.toml"))?;
    let mut packages = resolve_packages(&cfg, &repo_root);

    if let Some(only) = &args.only {
        packages.retain(|p| &p.name == only);
        if packages.is_empty() {
            eprintln!("unknown package: {only}");
            return Ok(ExitCode::from(2));
        }
    }

    let tg = TelegramNotifier::from_env();
    let run_id = notify::run_id();
    let mut results = Vec::with_capacity(packages.len());
    for pkg in &packages {
        println!("== {}", pkg.name);
        let res = process(pkg, args.dry_run);
        println!("   {}", res.render());
        results.push(res);
    }

    if cfg.telegram.enabled {
        tg.send_digest(&results, &run_id);
    }
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    match run_main() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vkey_orders_like_python_tuples() {
        assert!(vkey("1.0.0") < vkey("1.0.1"));
        assert!(vkey("1.2") < vkey("1.2.3")); // a shorter prefix is smaller
        assert!(vkey("2.0.0") > vkey("1.9.9"));
        assert_eq!(vkey("1.0.0"), vkey("1.0.0"));
    }

    #[test]
    fn vkey_strips_non_digits_per_token() {
        assert_eq!(vkey("1.2.3"), vec![1, 2, 3]);
        assert_eq!(vkey("v2.0"), vec![2, 0]); // leading non-digits dropped
        assert_eq!(vkey("1..2"), vec![1, 0, 2]); // empty token -> 0
    }

    #[test]
    fn matches_at_start_mirrors_re_match() {
        let re = Regex::new(VERSION_REGEX_DEFAULT).unwrap();
        assert!(matches_at_start(&re, "3.1.8"));
        assert!(!matches_at_start(&re, "v3.1.8"));
        assert!(!matches_at_start(&re, "3.1.8-beta"));

        // re.match anchors at the start only, not the end.
        let digits = Regex::new(r"\d+").unwrap();
        assert!(matches_at_start(&digits, "12ab"));
        assert!(!matches_at_start(&digits, "ab12"));
    }

    /// End-to-end test of `update_checksums` for a multi-arch PKGBUILD.
    ///
    /// A fake `makepkg` on PATH prints a deterministic sums block per CARCH,
    /// proving that update_checksums runs once per arch and splices each result
    /// into the matching `sha256sums_<arch>` line — the exact regression we fixed.
    #[test]
    fn update_checksums_multi_arch_splices_per_arch() {
        use std::io::Write;

        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        // Fake makepkg: echoes a sums line tagged with the CARCH it was called with.
        let bin = dir.join("makepkg");
        let script = "#!/bin/sh\n\
            # If invoked with -g, emit a per-arch sums block; else no-op.\n\
            for a in \"$@\"; do [ \"$a\" = \"-g\" ] && {\n\
                printf \"sha256sums=('hash-%s')\\n\" \"${CARCH:-unknown}\";\n\
                exit 0;\n\
            }; done\n";
        let mut f = fs::File::create(&bin).unwrap();
        f.write_all(script.as_bytes()).unwrap();
        drop(f);
        make_executable(&bin);

        // A PKGBUILD declaring two arches and per-arch sums.
        let pkgbuild = dir.join("PKGBUILD");
        fs::write(
            &pkgbuild,
            "pkgname=zcode-appimage\n\
             pkgver=3.2.2\n\
             arch=('x86_64' 'aarch64')\n\
             sha256sums_x86_64=('STALE')\n\
             sha256sums_aarch64=('STALE')\n",
        )
        .unwrap();

        // Run with the fake makepkg shadowing the real one.
        let old_path = env::var_os("PATH").unwrap();
        // SAFETY: no other test in this binary reads PATH, so there's no data race
        // with concurrent tests over this shared env var.
        unsafe {
            env::set_var("PATH", dir);
        }

        // `updpkgsums` should NOT be on this temp PATH, so we force the makepkg path.
        // Temporarily pretend updpkgsums is absent by also ensuring it's not found.
        let res = update_checksums(dir);

        unsafe {
            env::set_var("PATH", old_path);
        }

        res.unwrap();
        let text = fs::read_to_string(&pkgbuild).unwrap();
        assert!(
            text.contains("sha256sums_x86_64=('hash-x86_64')"),
            "x86_64 line not spliced: {text}"
        );
        assert!(
            text.contains("sha256sums_aarch64=('hash-aarch64')"),
            "aarch64 line not spliced: {text}"
        );
        assert!(!text.contains("'STALE'"), "stale sums left behind: {text}");
        assert!(
            !text.contains("\nsha256sums="),
            "a generic sha256sums= line should not be present: {text}"
        );
    }

    #[cfg(unix)]
    fn make_executable(p: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = fs::metadata(p).unwrap().permissions();
        perm.set_mode(0o755);
        fs::set_permissions(p, perm).unwrap();
    }
}
