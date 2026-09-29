//! Tailing event files on a remote host over ssh.
//!
//! Uses the system `ssh` binary, so ~/.ssh/config, keys, the agent, ProxyJump
//! etc. all work as usual. One multiplexed master connection is opened up
//! front (this is where a password / passphrase prompt can appear); each poll
//! then runs a tiny POSIX sh script over that connection which prints only
//! the bytes appended to each event file since the last poll.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct RemoteSpec {
    /// config name (or host) — used for labels and run-name prefixes
    pub name: String,
    pub host: String,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity: Option<String>,
    pub ssh_config: Option<String>,
    pub jump: Option<String>,
    pub ssh_args: Vec<String>,
    pub path: String,
}

impl RemoteSpec {
    pub fn display(&self) -> String {
        let user = self.user.as_ref().map(|u| format!("{u}@")).unwrap_or_default();
        format!("{user}{}:{}", self.name, self.path)
    }
}

pub struct Chunk {
    /// path relative to the remote root, e.g. "./exp1/events.out.tfevents…"
    pub path: String,
    pub data: Vec<u8>,
    /// the remote file shrank / was replaced; drop any partial record
    pub reset: bool,
}

type Batch = Result<Vec<Chunk>, String>;

pub struct Remote {
    rx: Receiver<Batch>,
    wake: Sender<()>,
}

impl Remote {
    /// Connect, fetch everything once (blocking), then keep polling in a
    /// background thread.
    pub fn start(spec: RemoteSpec, interval: Duration) -> anyhow::Result<(Remote, Vec<Chunk>)> {
        ensure_master(&spec, true).map_err(anyhow::Error::msg)?;
        let mut offsets = HashMap::new();
        let first = fetch(&spec, &mut offsets).map_err(anyhow::Error::msg)?;
        let (tx, rx) = mpsc::channel();
        let (wake, wake_rx) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            loop {
                if let Err(RecvTimeoutError::Disconnected) = wake_rx.recv_timeout(interval) {
                    break; // UI is gone
                }
                let mut batch = fetch(&spec, &mut offsets);
                if batch.is_err() && ensure_master(&spec, false).is_ok() {
                    batch = fetch(&spec, &mut offsets); // connection dropped: reconnect once
                }
                if tx.send(batch).is_err() {
                    break;
                }
            }
        });
        Ok((Remote { rx, wake }, first))
    }

    pub fn poll(&self) -> Vec<Batch> {
        self.rx.try_iter().collect()
    }

    pub fn wake(&self) {
        let _ = self.wake.send(());
    }
}

fn expand(p: &str) -> String {
    match (p.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest).display().to_string(),
        _ => p.to_string(),
    }
}

fn control_path() -> String {
    // %C = hash of (local host, remote host, port, user): short enough for
    // the unix socket path limit, unique per destination.
    "/tmp/tbtui-%C".to_string()
}

fn ssh(spec: &RemoteSpec) -> Command {
    let mut c = Command::new("ssh");
    if let Some(f) = &spec.ssh_config {
        c.arg("-F").arg(expand(f));
    }
    c.arg("-o").arg(format!("ControlPath={}", control_path()));
    c.args(["-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=3", "-o", "ConnectTimeout=15"]);
    if let Some(i) = &spec.identity {
        c.arg("-i").arg(expand(i)).args(["-o", "IdentitiesOnly=yes"]);
    }
    if let Some(p) = spec.port {
        c.arg("-p").arg(p.to_string());
    }
    if let Some(u) = &spec.user {
        c.arg("-l").arg(u);
    }
    if let Some(j) = &spec.jump {
        c.arg("-J").arg(j);
    }
    c.args(&spec.ssh_args);
    c.stdin(Stdio::null());
    c
}

/// Make sure a background master connection exists. With `interactive`,
/// ssh may ask for a password / passphrase / host-key confirmation on the
/// terminal (ssh talks to /dev/tty directly for that).
fn ensure_master(spec: &RemoteSpec, interactive: bool) -> Result<(), String> {
    let running = ssh(spec)
        .args(["-O", "check"])
        .arg(&spec.host)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if running {
        return Ok(());
    }
    // ssh's own messages go to a log file (-E): the forked master keeps its
    // stderr open for its whole life, which must not be the TUI's terminal.
    let log = std::env::temp_dir().join(format!("tbtui-ssh-{}.log", std::process::id()));
    let _ = std::fs::remove_file(&log);
    let mut c = ssh(spec);
    c.args(["-o", "ControlMaster=yes", "-o", "ControlPersist=600", "-C", "-f", "-N", "-E"])
        .arg(&log)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if !interactive {
        c.args(["-o", "BatchMode=yes"]);
    }
    let status = c.arg(&spec.host).status().map_err(|e| format!("cannot run ssh: {e}"))?;
    let msg = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = std::fs::remove_file(&log);
    if status.success() {
        Ok(())
    } else {
        let detail = msg.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
        Err(format!(
            "ssh to {} failed: {}",
            spec.display(),
            if detail.is_empty() { status.to_string() } else { detail }
        ))
    }
}

/// Single-quote for POSIX sh.
fn sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn build_script(root: &str, offsets: &HashMap<String, u64>) -> String {
    let cd = if root == "~" {
        "~".to_string()
    } else if let Some(rest) = root.strip_prefix("~/") {
        format!("~/{}", sq(rest))
    } else {
        sq(root)
    };
    let mut s = String::new();
    let _ = writeln!(s, "cd {cd} 2>/dev/null || {{ echo 'no such directory' >&2; exit 3; }}");
    s.push_str("find -L . -type f -name '*tfevents*' 2>/dev/null | while IFS= read -r f; do\n  case \"$f\" in\n");
    for (path, off) in offsets {
        let _ = writeln!(s, "    {}) off={off};;", sq(path));
    }
    s.push_str(
        r#"    *) off=0;;
  esac
  sz=$(wc -c < "$f" 2>/dev/null | tr -d ' ')
  [ -n "$sz" ] || continue
  [ "$sz" -lt "$off" ] && off=0
  n=$((sz - off))
  [ "$n" -gt 0 ] || continue
  printf 'F %s %s %s\n' "$off" "$n" "$f"
  tail -c +$((off + 1)) "$f" | head -c "$n"
done
"#,
    );
    s
}

fn fetch(spec: &RemoteSpec, offsets: &mut HashMap<String, u64>) -> Batch {
    let script = build_script(&spec.path, offsets);
    let mut child = ssh(spec)
        .args(["-o", "BatchMode=yes", "-o", "ControlMaster=no"])
        .arg(&spec.host)
        .arg("sh -s")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run ssh: {e}"))?;
    let mut stdin = child.stdin.take().unwrap();
    // write from a thread so a large script can't deadlock against output
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(script.as_bytes());
    });
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let _ = writer.join();
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
        return Err(match out.status.code() {
            Some(3) => format!("{}: no such directory on the remote host", spec.display()),
            _ => format!("{}: {}", spec.display(), if last.is_empty() { "ssh failed" } else { last }),
        });
    }
    Ok(parse_output(&out.stdout, offsets))
}

fn parse_output(buf: &[u8], offsets: &mut HashMap<String, u64>) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let mut pos = 0;
    while pos < buf.len() {
        let Some(nl) = buf[pos..].iter().position(|&b| b == b'\n').map(|i| pos + i) else { break };
        let header = String::from_utf8_lossy(&buf[pos..nl]).into_owned();
        let mut parts = header.splitn(4, ' ');
        let (Some("F"), Some(off), Some(n), Some(path)) = (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            break;
        };
        let (Ok(off), Ok(n)) = (off.parse::<u64>(), n.parse::<usize>()) else { break };
        let start = nl + 1;
        let end = start.saturating_add(n).min(buf.len());
        let data = buf[start..end].to_vec();
        let got = data.len();
        let prev = offsets.get(path).copied().unwrap_or(0);
        offsets.insert(path.to_string(), off + got as u64);
        chunks.push(Chunk { path: path.to_string(), data, reset: off < prev });
        pos = end;
        if got < n {
            break; // truncated output; resume from what we have next time
        }
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(sq("a'b"), r"'a'\''b'");
    }

    #[test]
    fn parses_stream() {
        let mut offs = HashMap::new();
        offs.insert("./a/e.tfevents".to_string(), 10);
        let out = b"F 10 3 ./a/e.tfevents\nxyzF 0 2 ./b c/e.tfevents\nhi";
        let c = parse_output(out, &mut offs);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].data, b"xyz");
        assert!(!c[0].reset);
        assert_eq!(c[1].path, "./b c/e.tfevents");
        assert_eq!(offs["./a/e.tfevents"], 13);
        assert_eq!(offs["./b c/e.tfevents"], 2);
    }

    #[test]
    fn script_runs_locally() {
        // exercise the generated shell script against a local directory
        let dir = std::env::temp_dir().join(format!("tbtui-script-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("run a")).unwrap();
        std::fs::write(dir.join("run a/events.out.tfevents.1"), b"0123456789").unwrap();
        let mut offs = HashMap::new();
        offs.insert("./run a/events.out.tfevents.1".to_string(), 4);
        let script = build_script(dir.to_str().unwrap(), &offs);
        let out = Command::new("sh").arg("-c").arg(&script).output().unwrap();
        let c = parse_output(&out.stdout, &mut offs);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].data, b"456789");
        assert_eq!(offs["./run a/events.out.tfevents.1"], 10);
    }
}
