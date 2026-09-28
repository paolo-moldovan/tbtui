//! Saved sessions: everything needed to reopen tbtui exactly as it was left.
//!
//! One JSON file per set of log targets, under
//! `$XDG_STATE_HOME/tbtui/sessions/` (default `~/.local/state/tbtui/sessions/`).
//! `last` in the state dir names the most recently used session (`tbtui -c`).

use crate::filter::GrexOpts;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(default)]
pub struct SshSaved {
    pub config: Option<PathBuf>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity: Option<String>,
    pub ssh_config: Option<String>,
    pub jump: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
#[serde(default)]
pub struct FilterSaved {
    pub text: String,
    pub examples: Vec<String>,
    pub grex: GrexOpts,
}

#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
#[serde(default)]
pub struct TreeSaved {
    pub filter: FilterSaved,
    pub collapsed: Vec<String>,
    /// path of the highlighted row
    pub cursor: Option<String>,
    pub offset: usize,
}

/// UI state. Every field has a default so older/newer files still load.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct UiState {
    pub smoothing: f64,
    pub log_y: bool,
    /// "step" | "time"
    pub x_axis: String,
    pub x_range: Option<(f64, f64)>,
    pub cursor: Option<f64>,
    pub ignore_outliers: bool,
    pub show_raw: bool,
    pub markers: bool,
    pub palette: String,
    pub tags: TreeSaved,
    pub runs: TreeSaved,
    pub pinned: Vec<String>,
    pub hidden: Vec<String>,
    pub colors: BTreeMap<String, usize>,
    /// "tags" | "runs" | "charts"
    pub focus: String,
    pub show_all: bool,
    pub sidebar: bool,
    pub side_w: Option<u16>,
    pub split_pct: u16,
    pub grid_cols: u16,
    pub focus_panel: usize,
    pub live: bool,
    pub interval: f64,
}

impl Default for UiState {
    fn default() -> Self {
        UiState {
            smoothing: 0.6,
            log_y: false,
            x_axis: "step".into(),
            x_range: None,
            cursor: None,
            ignore_outliers: false,
            show_raw: true,
            markers: true,
            palette: "okabe-ito".into(),
            tags: TreeSaved::default(),
            runs: TreeSaved::default(),
            pinned: Vec::new(),
            hidden: Vec::new(),
            colors: BTreeMap::new(),
            focus: "tags".into(),
            show_all: false,
            sidebar: true,
            side_w: None,
            split_pct: 60,
            grid_cols: 0,
            focus_panel: 0,
            live: true,
            interval: 2.0,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(default)]
pub struct Session {
    pub version: u32,
    /// command-line targets, local paths made absolute
    pub targets: Vec<String>,
    pub ssh: SshSaved,
    pub saved_at: u64,
    pub ui: UiState,
}

pub fn state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("tbtui")
}

/// Local targets become absolute so a session can be reopened from anywhere.
pub fn normalize_targets(args: &[String]) -> Vec<String> {
    args.iter()
        .map(|a| match Path::new(a).canonicalize() {
            Ok(p) => p.display().to_string(),
            Err(_) => a.clone(),
        })
        .collect()
}

fn fnv1a(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
}

/// Stable, readable file name for a set of targets.
pub fn key(targets: &[String]) -> String {
    let mut sorted = targets.to_vec();
    sorted.sort();
    let joined = sorted.join("\n");
    let stem: String = sorted
        .first()
        .map(|t| t.trim_end_matches('/').rsplit(['/', ':']).next().unwrap_or("").to_string())
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .take(40)
        .collect();
    format!("{}-{:016x}", if stem.is_empty() { "root" } else { &stem }, fnv1a(&joined))
}

fn session_path(key: &str) -> PathBuf {
    state_dir().join("sessions").join(format!("{key}.json"))
}

pub fn load(key: &str) -> Option<Session> {
    let s = std::fs::read_to_string(session_path(key)).ok()?;
    serde_json::from_str(&s).ok()
}

pub fn load_last() -> Option<Session> {
    let key = std::fs::read_to_string(state_dir().join("last")).ok()?;
    load(key.trim())
}

pub fn save(key: &str, s: &Session) -> std::io::Result<()> {
    let path = session_path(key);
    std::fs::create_dir_all(path.parent().unwrap())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(s)?)?;
    std::fs::rename(&tmp, &path)?; // atomic: never leaves a half-written file
    std::fs::write(state_dir().join("last"), key)
}

/// All saved sessions, newest first.
pub fn list() -> Vec<(String, Session)> {
    let mut v: Vec<(String, Session)> = std::fs::read_dir(state_dir().join("sessions"))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| {
            let key = e.path().file_stem()?.to_string_lossy().into_owned();
            let s: Session = serde_json::from_str(&std::fs::read_to_string(e.path()).ok()?).ok()?;
            Some((key, s))
        })
        .collect();
    v.sort_by_key(|s| std::cmp::Reverse(s.1.saved_at));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_is_order_independent_and_readable() {
        let a = key(&["/x/runs".into(), "gpu1:~/tb".into()]);
        let b = key(&["gpu1:~/tb".into(), "/x/runs".into()]);
        assert_eq!(a, b);
        assert!(a.starts_with("runs-"), "{a}");
    }

    #[test]
    fn roundtrip_and_defaults() {
        let mut s = Session { version: 1, targets: vec!["/a".into()], ..Default::default() };
        s.ui.pinned = vec!["loss".into()];
        s.ui.x_range = Some((10.0, 20.0));
        let back: Session = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.ui, s.ui);
        // unknown / missing fields don't break loading
        let partial: Session = serde_json::from_str(r#"{"targets":["/a"],"ui":{"log_y":true,"future":1}}"#).unwrap();
        assert!(partial.ui.log_y);
        assert_eq!(partial.ui.smoothing, 0.6);
    }
}
