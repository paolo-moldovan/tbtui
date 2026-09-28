//! `~/.config/tbtui/config.toml`: named ssh remotes, and resolving command
//! line targets (local dirs, `host:path`, `user@host:path`, or a remote name).

use crate::remote::RemoteSpec;
use crate::store::Target;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const TEMPLATE: &str = r#"# tbtui config
#
# Each [remotes.NAME] lets you run `tbtui NAME` (uses `path` below) or
# `tbtui NAME:/some/other/logdir`. Everything is passed to the system `ssh`,
# so anything in ~/.ssh/config (Host aliases, keys, ProxyJump…) also works.
# All fields are optional except that a log path must come from here or the
# command line.

# palette = "okabe-ito"              # run colors: okabe-ito | tol-bright (colorblind-safe) | vivid

# [remotes.gpu1]
# host = "10.0.0.5"                  # IP / hostname / ~/.ssh/config Host alias (default: NAME)
# user = "paolo"
# port = 22
# path = "~/experiments/runs"        # default log dir on that machine
# identity_file = "~/.ssh/id_ed25519"
# ssh_config = "~/.ssh/config"       # passed as `ssh -F`
# jump = "user@bastion.example.com"  # ProxyJump (`ssh -J`)
# ssh_args = ["-o", "StrictHostKeyChecking=accept-new"]

# [remotes.cluster]                  # minimal: host alias already in ~/.ssh/config
# path = "/scratch/paolo/tb"
"#;

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// run color palette: okabe-ito (default), tol-bright, vivid
    pub palette: Option<String>,
    #[serde(default)]
    pub remotes: BTreeMap<String, RemoteCfg>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct RemoteCfg {
    pub host: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub path: Option<String>,
    pub identity_file: Option<String>,
    pub ssh_config: Option<String>,
    pub jump: Option<String>,
    #[serde(default)]
    pub ssh_args: Vec<String>,
}

/// Command-line ssh overrides (take precedence over the config file).
#[derive(Default)]
pub struct SshOverrides {
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity: Option<String>,
    pub ssh_config: Option<String>,
    pub jump: Option<String>,
}

pub fn default_path() -> PathBuf {
    if let Some(p) = std::env::var_os("TBTUI_CONFIG") {
        return PathBuf::from(p);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("tbtui").join("config.toml")
}

/// Load the config. A missing default file is fine; a missing explicit one is not.
pub fn load(explicit: Option<&Path>) -> anyhow::Result<Config> {
    let path = explicit.map(Path::to_path_buf).unwrap_or_else(default_path);
    match std::fs::read_to_string(&path) {
        Ok(s) => toml::from_str(&s).map_err(|e| anyhow::anyhow!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && explicit.is_none() => Ok(Config::default()),
        Err(e) => Err(anyhow::anyhow!("{}: {e}", path.display())),
    }
}

/// Turn a command-line argument into a local or remote target.
///
/// - an existing local path is always local
/// - `NAME` where NAME is a configured remote → that remote's default path
/// - `[user@]host:path` (scp-style) → remote; `host` may be a configured name
pub fn resolve(arg: &str, cfg: &Config, ov: &SshOverrides) -> anyhow::Result<Target> {
    if Path::new(arg).exists() {
        return Ok(Target::Local(PathBuf::from(arg)));
    }
    let (dest, path) = match arg.split_once(':') {
        Some((d, p)) if !d.is_empty() && !d.contains('/') => (d, Some(p)),
        _ if cfg.remotes.contains_key(arg) => (arg, None),
        _ => anyhow::bail!("{arg} does not exist (and is not a configured remote or host:path)"),
    };
    let (user, name) = match dest.split_once('@') {
        Some((u, h)) => (Some(u.to_string()), h),
        None => (None, dest),
    };
    let rc = cfg.remotes.get(name).cloned().unwrap_or_default();
    let path = path
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .or(rc.path)
        .ok_or_else(|| anyhow::anyhow!("no log path for {name}: use {name}:/path/to/logs or set `path` in its config entry"))?;
    Ok(Target::Remote(RemoteSpec {
        name: name.to_string(),
        host: rc.host.unwrap_or_else(|| name.to_string()),
        user: ov.user.clone().or(user).or(rc.user),
        port: ov.port.or(rc.port),
        identity: ov.identity.clone().or(rc.identity_file),
        ssh_config: ov.ssh_config.clone().or(rc.ssh_config),
        jump: ov.jump.clone().or(rc.jump),
        ssh_args: rc.ssh_args,
        path,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        toml::from_str(
            r#"
            [remotes.gpu1]
            host = "10.0.0.5"
            user = "paolo"
            path = "~/runs"
            "#,
        )
        .unwrap()
    }

    fn remote(t: Target) -> RemoteSpec {
        match t {
            Target::Remote(r) => r,
            Target::Local(p) => panic!("expected remote, got {}", p.display()),
        }
    }

    #[test]
    fn template_parses() {
        let c: Config = toml::from_str(TEMPLATE).unwrap();
        assert!(c.remotes.is_empty());
    }

    #[test]
    fn named_remote() {
        let r = remote(resolve("gpu1", &cfg(), &SshOverrides::default()).unwrap());
        assert_eq!((r.host.as_str(), r.user.as_deref(), r.path.as_str()), ("10.0.0.5", Some("paolo"), "~/runs"));
        let r = remote(resolve("gpu1:/data/tb", &cfg(), &SshOverrides::default()).unwrap());
        assert_eq!(r.path, "/data/tb");
    }

    #[test]
    fn adhoc_and_overrides() {
        let ov = SshOverrides { port: Some(2222), ..Default::default() };
        let r = remote(resolve("bob@box:/logs", &cfg(), &ov).unwrap());
        assert_eq!((r.host.as_str(), r.user.as_deref(), r.port), ("box", Some("bob"), Some(2222)));
        assert!(resolve("box:", &cfg(), &ov).is_err()); // no path anywhere
        assert!(resolve("nonexistent/dir", &cfg(), &ov).is_err());
    }
}
