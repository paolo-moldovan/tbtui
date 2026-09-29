//! `tbtui upgrade`: check crates.io for a newer release and install it.
//!
//! No HTTP client is linked in (the binary stays small): the version check
//! goes through `cargo search`, and the upgrade is `cargo install`, which
//! replaces the binary only when a newer version exists. tbtui upgrades itself
//! only when it was installed by cargo; for other installs (Homebrew, a
//! downloaded binary) it says how to upgrade instead of leaving a second copy.

use anyhow::{Context, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
const CRATE: &str = env!("CARGO_PKG_NAME");
const RELEASES: &str = concat!(env!("CARGO_PKG_REPOSITORY"), "/releases");

/// `1.2.3` (an optional `-pre` / `+build` suffix is ignored) as a comparable tuple.
fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.trim().trim_start_matches('v').split(['-', '+']).next()?;
    let mut it = core.split('.').map(|p| p.parse::<u64>());
    Some((it.next()?.ok()?, it.next()?.ok()?, it.next()?.ok()?))
}

/// Pull the version out of a `cargo search` line: `tbtui = "0.2.1"    # description`.
fn parse_search(out: &str, name: &str) -> Option<String> {
    let line = out.lines().find(|l| l.split_whitespace().next() == Some(name))?;
    let start = line.find('"')? + 1;
    let end = start + line[start..].find('"')?;
    Some(line[start..end].to_string())
}

fn latest_version() -> anyhow::Result<String> {
    let out = Command::new("cargo")
        .args(["search", CRATE, "--limit", "1"])
        .output()
        .context("could not run `cargo` (is Rust installed?)")?;
    if !out.status.success() {
        bail!("`cargo search` failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    parse_search(&String::from_utf8_lossy(&out.stdout), CRATE)
        .with_context(|| format!("{CRATE} was not found on crates.io"))
}

fn cargo_bin_dir() -> Option<PathBuf> {
    let home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")))?;
    Some(home.join("bin"))
}

fn installed_by_cargo(exe: &Path) -> bool {
    let (Some(dir), Some(exe_dir)) = (cargo_bin_dir(), exe.parent()) else { return false };
    match (dir.canonicalize(), exe_dir.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn other_install_hint(exe: &Path) -> String {
    let p = exe.display().to_string();
    if p.contains("/Cellar/") || p.contains("/homebrew/") || p.contains("/linuxbrew/") {
        return format!("installed with Homebrew: run `brew upgrade {CRATE}`");
    }
    format!(
        "{p} was not installed by cargo, so tbtui will not replace it.\n\
         Upgrade the way you installed it, or switch to cargo:\n  \
         cargo install {CRATE} --locked\n  \
         cargo binstall {CRATE}        (prebuilt binary)\n  \
         or download a build from {RELEASES}"
    )
}

pub fn run(check_only: bool) -> anyhow::Result<()> {
    println!("tbtui {CURRENT} · checking crates.io …");
    let latest = latest_version()?;
    let newer = match (parse_version(&latest), parse_version(CURRENT)) {
        (Some(l), Some(c)) => l > c,
        _ => latest != CURRENT,
    };
    if !newer {
        println!("already up to date ({CURRENT})");
        return Ok(());
    }
    println!("new version available: {CURRENT} → {latest}");
    println!("changes: {RELEASES}/tag/v{latest}");
    if check_only {
        println!("run `tbtui upgrade` to install it");
        return Ok(());
    }
    let exe = std::env::current_exe().context("cannot locate the running binary")?;
    if !installed_by_cargo(&exe) {
        println!("{}", other_install_hint(&exe));
        return Ok(());
    }
    println!("running: cargo install {CRATE} --locked\n");
    let status =
        Command::new("cargo").args(["install", CRATE, "--locked"]).status().context("could not run `cargo install`")?;
    if !status.success() {
        bail!("cargo install failed ({status})");
    }
    println!("\nupgraded to {latest}. Saved sessions and your config carry over unchanged.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert!(parse_version("0.10.0") > parse_version("0.9.9"));
        assert!(parse_version("1.0.0") > parse_version("0.99.99"));
        assert_eq!(parse_version("v1.2.3-rc.1+abc"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.2"), None);
    }

    #[test]
    fn parses_cargo_search() {
        let out = "tbtui = \"0.2.1\"    # TensorBoard scalars in your terminal\n\
                   tbtui-extra = \"9.9.9\"    # something else\n\
                   ... and 3 crates more (use --limit N to see more)\n";
        assert_eq!(parse_search(out, "tbtui").as_deref(), Some("0.2.1"));
        assert_eq!(parse_search("error: nothing", "tbtui"), None);
    }

    #[test]
    fn non_cargo_installs_get_a_hint() {
        assert!(other_install_hint(Path::new("/opt/homebrew/Cellar/tbtui/0.1.0/bin/tbtui")).contains("brew upgrade"));
        assert!(other_install_hint(Path::new("/usr/local/bin/tbtui")).contains("cargo install"));
    }
}
