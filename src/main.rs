//! tbtui: TensorBoard scalars in your terminal.
//!
//! Module map: `events` decodes TFRecord/protobuf event files, `store` tails
//! them (locally or through `remote`, which runs `ssh`), `plot` and `tree`
//! render, `app` is the interactive UI, `snapshot` the one-shot output,
//! `filter` the regex/grex filters, `session` persists UI state, `config`
//! reads `config.toml`, and `upgrade` implements `tbtui upgrade`.

mod app;
mod config;
mod events;
mod filter;
mod plot;
mod remote;
mod session;
mod snapshot;
mod store;
mod tree;
mod upgrade;

use clap::parser::ValueSource;
use clap::{ArgMatches, Args, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use regex::{Regex, RegexBuilder};
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use store::Target;

/// TensorBoard scalars in your terminal.
///
/// Point it at a log directory (searched recursively for *tfevents* files,
/// one run per directory, like TensorBoard) to get a live, interactive view.
///
/// Remote logs over ssh work the same way: `tbtui user@host:~/runs`, or just
/// `tbtui NAME` for a remote defined in the config file (see `tbtui config`).
#[derive(Parser)]
#[command(version, args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,

    /// Log dirs / event files, `[user@]host:path` over ssh, or a configured remote name
    /// (default: current directory)
    #[arg(value_name = "LOGDIR")]
    logdir: Vec<String>,

    /// Same as the positional LOGDIR (TensorBoard-compatible spelling)
    #[arg(long = "logdir", value_name = "LOGDIR")]
    logdir_flag: Vec<String>,

    /// Seconds between checks for new data
    #[arg(short = 'n', long, default_value_t = 2.0)]
    interval: f64,

    /// Initial tag filter (regex)
    #[arg(short, long)]
    filter: Option<String>,

    /// Initial run filter (regex)
    #[arg(short = 'R', long)]
    run_filter: Option<String>,

    /// Reopen the last session (its logs, panes, filters, zoom… exactly as left)
    #[arg(short = 'c', long = "continue")]
    continue_last: bool,

    /// Ignore the saved state for these logs (it is overwritten on exit)
    #[arg(long)]
    fresh: bool,

    #[command(flatten)]
    plot: PlotArgs,

    #[command(flatten)]
    ssh: SshArgs,
}

/// ssh options; override the config file and apply to every remote target.
#[derive(Args, Clone)]
struct SshArgs {
    /// Config file [default: ~/.config/tbtui/config.toml, or $TBTUI_CONFIG]
    #[arg(long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,
    /// ssh user
    #[arg(short = 'l', long, global = true, help_heading = "SSH")]
    user: Option<String>,
    /// ssh port
    #[arg(short = 'p', long, global = true, help_heading = "SSH")]
    port: Option<u16>,
    /// ssh private key file
    #[arg(short = 'i', long, global = true, value_name = "FILE", help_heading = "SSH")]
    identity: Option<String>,
    /// ssh config file (ssh -F)
    #[arg(short = 'F', long, global = true, value_name = "FILE", help_heading = "SSH")]
    ssh_config: Option<String>,
    /// jump host (ssh -J)
    #[arg(short = 'J', long, global = true, value_name = "[USER@]HOST", help_heading = "SSH")]
    jump: Option<String>,
}

impl SshArgs {
    fn to_saved(&self) -> session::SshSaved {
        session::SshSaved {
            config: self.config.clone(),
            user: self.user.clone(),
            port: self.port,
            identity: self.identity.clone(),
            ssh_config: self.ssh_config.clone(),
            jump: self.jump.clone(),
        }
    }

    /// Use saved ssh settings for anything not given on this command line.
    fn fill_from(&mut self, s: &session::SshSaved) {
        self.config = self.config.take().or(s.config.clone());
        self.user = self.user.take().or(s.user.clone());
        self.port = self.port.or(s.port);
        self.identity = self.identity.take().or(s.identity.clone());
        self.ssh_config = self.ssh_config.take().or(s.ssh_config.clone());
        self.jump = self.jump.take().or(s.jump.clone());
    }

    fn overrides(&self) -> config::SshOverrides {
        config::SshOverrides {
            user: self.user.clone(),
            port: self.port,
            identity: self.identity.clone(),
            ssh_config: self.ssh_config.clone(),
            jump: self.jump.clone(),
        }
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Print charts once and exit (no interactivity) — handy over ssh or in CI logs
    #[command(visible_alias = "snap")]
    Snapshot(SnapCmd),
    /// List runs and tags with their latest values
    Ls {
        #[arg(value_name = "LOGDIR")]
        logdir: Vec<String>,
        /// Only tags matching this regex
        #[arg(short, long)]
        tags: Option<String>,
        #[arg(long)]
        no_color: bool,
    },
    /// Write fake training runs (for trying tbtui out)
    Demo {
        /// Output directory
        dir: PathBuf,
        /// Keep appending points (simulates a live training job)
        #[arg(long)]
        live: bool,
    },
    /// List saved sessions (reopen one with `tbtui <its targets>` or the last with `tbtui -c`)
    Sessions,
    /// Check for a newer release on crates.io and install it (cargo installs only)
    Upgrade {
        /// Only report whether a newer version exists
        #[arg(long)]
        check: bool,
    },
    /// Show the config file location and configured remotes
    Config {
        /// Write a commented example config if none exists
        #[arg(long)]
        init: bool,
    },
}

#[derive(Args)]
struct SnapCmd {
    #[arg(value_name = "LOGDIR")]
    logdir: Vec<String>,
    /// Only tags matching this regex (e.g. 'loss')
    #[arg(short, long)]
    tags: Option<String>,
    /// Only runs matching this regex
    #[arg(short, long)]
    runs: Option<String>,
    /// Total width in columns (default: terminal width)
    #[arg(short = 'W', long)]
    width: Option<u16>,
    /// Height of each chart in rows
    #[arg(short = 'H', long, default_value_t = 20)]
    height: u16,
    /// Charts per row
    #[arg(short, long, default_value_t = 1)]
    cols: u16,
    #[arg(long)]
    no_color: bool,
    #[command(flatten)]
    plot: PlotArgs,
}

#[derive(Args, Clone)]
struct PlotArgs {
    /// EMA smoothing weight in [0, 1), like TensorBoard's slider
    #[arg(short, long, default_value_t = 0.6)]
    smoothing: f64,
    /// Logarithmic y-axis
    #[arg(long)]
    log_y: bool,
    /// X-axis
    #[arg(short = 'x', long, value_enum, default_value_t = XArg::Step)]
    x_axis: XArg,
    /// Only use 256 colors (auto-detected from $COLORTERM otherwise)
    #[arg(long, global = true)]
    no_truecolor: bool,
    /// Run colors: okabe-ito and tol-bright are colorblind-safe [default: okabe-ito]
    #[arg(long, global = true, value_parser = ["okabe-ito", "tol-bright", "vivid"])]
    palette: Option<String>,
    /// Scale the y-axis to the 5th–95th percentile, ignoring spikes (toggle with `o`)
    #[arg(short = 'O', long, global = true)]
    ignore_outliers: bool,
    /// Don't draw per-run shape markers on the lines
    #[arg(long, global = true)]
    no_markers: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum XArg {
    Step,
    Time,
}

impl PlotArgs {
    /// The command-line palette wins over the config file's.
    fn apply_palette(&self) {
        if let Some(p) = &self.palette {
            plot::set_palette(p);
        }
    }

    fn opts(&self) -> plot::PlotOpts {
        plot::PlotOpts {
            smoothing: self.smoothing.clamp(0.0, 0.999),
            log_y: self.log_y,
            xmode: match self.x_axis {
                XArg::Step => plot::XMode::Step,
                XArg::Time => plot::XMode::Relative,
            },
            markers: !self.no_markers,
            ignore_outliers: self.ignore_outliers,
            ..Default::default()
        }
    }
}

fn regex(s: &Option<String>) -> anyhow::Result<Option<Regex>> {
    Ok(match s {
        Some(s) => Some(RegexBuilder::new(s).case_insensitive(true).build()?),
        None => None,
    })
}

/// Resolve command-line targets and open the data store (connecting to
/// remotes, which may prompt for a password before any UI is shown).
fn open_store(args: Vec<String>, ssh: &SshArgs, interval: Duration) -> anyhow::Result<(store::Store, String)> {
    let args = if args.is_empty() { vec![".".to_string()] } else { args };
    let cfg = config::load(ssh.config.as_deref())?;
    if let Some(p) = &cfg.palette
        && !plot::set_palette(p)
    {
        anyhow::bail!("unknown palette {p:?} in config (okabe-ito, tol-bright, vivid)");
    }
    let ov = ssh.overrides();
    let targets = args.iter().map(|a| config::resolve(a, &cfg, &ov)).collect::<anyhow::Result<Vec<Target>>>()?;
    let label = targets
        .iter()
        .map(|t| match t {
            Target::Local(p) => p.display().to_string(),
            Target::Remote(r) => r.display(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    Ok((store::Store::open(targets, interval)?, label))
}

fn show_config(init: bool) -> anyhow::Result<()> {
    let path = config::default_path();
    if init {
        if path.exists() {
            anyhow::bail!("{} already exists", path.display());
        }
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, config::TEMPLATE)?;
        println!("wrote {}", path.display());
        return Ok(());
    }
    if !path.exists() {
        println!("no config at {} (create one with `tbtui config --init`)", path.display());
        return Ok(());
    }
    let cfg = config::load(Some(&path))?;
    println!("{}", path.display());
    if cfg.remotes.is_empty() {
        println!("  no remotes defined");
    }
    for (name, r) in &cfg.remotes {
        let user = r.user.as_ref().map(|u| format!("{u}@")).unwrap_or_default();
        let host = r.host.as_deref().unwrap_or(name);
        let port = r.port.map(|p| format!(":{p}")).unwrap_or_default();
        println!("  {name:<12} {user}{host}{port}  {}", r.path.as_deref().unwrap_or("(no default path)"));
    }
    Ok(())
}

/// Was this option given on the command line (vs. its default)?
fn explicit(m: &ArgMatches, id: &str) -> bool {
    m.try_get_raw(id).is_ok() && m.value_source(id) == Some(ValueSource::CommandLine)
}

fn list_sessions() {
    let sessions = session::list();
    if sessions.is_empty() {
        println!("no saved sessions yet ({})", session::state_dir().display());
        return;
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    for (i, (_, s)) in sessions.iter().enumerate() {
        let ago = plot::fmt_dur(now.saturating_sub(s.saved_at) as f64);
        let pinned = if s.ui.pinned.is_empty() { String::new() } else { format!(", {} pinned", s.ui.pinned.len()) };
        let last = if i == 0 { "  ← tbtui -c" } else { "" };
        println!("{ago:>8} ago  tbtui {}{pinned}{last}", s.targets.join(" "));
    }
}

fn main() -> anyhow::Result<()> {
    let matches = Cli::command().get_matches();
    let cli = Cli::from_arg_matches(&matches)?;
    match cli.cmd {
        Some(Cmd::Snapshot(s)) => {
            plot::init_colors(s.plot.no_truecolor);
            let (store, _) = open_store(s.logdir, &cli.ssh, Duration::from_secs(3600))?;
            s.plot.apply_palette();
            let tty = std::io::stdout().is_terminal();
            let term_w = ratatui::crossterm::terminal::size().map(|(w, _)| w).unwrap_or(100);
            let args = snapshot::SnapArgs {
                tags: regex(&s.tags)?,
                runs: regex(&s.runs)?,
                width: s.width.unwrap_or(if tty { term_w } else { 100 }).max(20),
                height: s.height.max(6),
                cols: s.cols,
                color: !s.no_color && std::env::var_os("NO_COLOR").is_none(),
                opts: s.plot.opts(),
            };
            snapshot::run(&store, &args)
        }
        Some(Cmd::Ls { logdir, tags, no_color }) => {
            plot::init_colors(false);
            let (store, _) = open_store(logdir, &cli.ssh, Duration::from_secs(3600))?;
            snapshot::list(&store, regex(&tags)?.as_ref(), !no_color && std::env::var_os("NO_COLOR").is_none());
            Ok(())
        }
        Some(Cmd::Demo { dir, live }) => demo(&dir, live),
        Some(Cmd::Config { init }) => show_config(init),
        Some(Cmd::Upgrade { check }) => upgrade::run(check),
        Some(Cmd::Sessions) => {
            list_sessions();
            Ok(())
        }
        None => {
            plot::init_colors(cli.plot.no_truecolor);
            let mut args: Vec<String> = cli.logdir.iter().chain(&cli.logdir_flag).cloned().collect();
            let mut ssh = cli.ssh.clone();
            let mut restored = None;
            if cli.continue_last {
                let s = session::load_last().ok_or_else(|| anyhow::anyhow!("no saved session to continue"))?;
                if args.is_empty() {
                    args = s.targets.clone();
                }
                ssh.fill_from(&s.ssh);
                restored = Some(s);
            }
            if args.is_empty() {
                args.push(".".into());
            }
            let targets = session::normalize_targets(&args);
            let key = session::key(&targets);
            if restored.is_none() && !cli.fresh {
                restored = session::load(&key);
                if let Some(s) = &restored {
                    ssh.fill_from(&s.ssh);
                }
            }
            let interval = Duration::from_secs_f64(cli.interval.max(0.1));
            let (store, label) = open_store(targets.clone(), &ssh, interval)?;
            let mut app = app::App::new(
                store,
                label,
                cli.plot.opts(),
                interval,
                cli.filter.as_deref().unwrap_or_default(),
                cli.run_filter.as_deref().unwrap_or_default(),
            );
            if let Some(s) = &restored {
                app.apply_state(&s.ui);
                // options typed on this command line win over the saved state
                let o = cli.plot.opts();
                let m = &matches;
                let opts = app.opts_mut();
                if explicit(m, "smoothing") {
                    opts.smoothing = o.smoothing;
                }
                if explicit(m, "log_y") {
                    opts.log_y = true;
                }
                if explicit(m, "x_axis") {
                    opts.xmode = o.xmode;
                    opts.x_range = None;
                    opts.cursor = None;
                }
                if explicit(m, "ignore_outliers") {
                    opts.ignore_outliers = true;
                }
                if explicit(m, "no_markers") {
                    opts.markers = false;
                }
                if explicit(m, "interval") {
                    app.set_interval(interval);
                }
                app.set_filters(
                    cli.filter.as_deref().filter(|_| explicit(m, "filter")),
                    cli.run_filter.as_deref().filter(|_| explicit(m, "run_filter")),
                );
            }
            cli.plot.apply_palette();
            app.set_session(app::SessionMeta { key, targets, ssh: ssh.to_saved() });
            let mut term = ratatui::init();
            let res = app.run(&mut term);
            ratatui::restore();
            res
        }
    }
}

// ---------------------------------------------------------------- demo data

fn demo(dir: &std::path::Path, live: bool) -> anyhow::Result<()> {
    let now = || SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64();
    let t0 = now();
    // tiny deterministic PRNG so there is no rand dependency
    let mut seed = 0x9e3779b97f4a7c15u64 ^ (t0 as u64);
    let mut noise = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 11) as f64 / (1u64 << 53) as f64 - 0.5
    };
    let runs =
        [("lr_1e-3/seed0", 1.0, 0.0), ("lr_1e-3/seed1", 1.0, 0.03), ("lr_3e-4", 0.5, 0.0), ("lr_1e-2", 2.2, 0.08)];
    let mut files = Vec::new();
    for (name, _, _) in &runs {
        let d = dir.join(name);
        std::fs::create_dir_all(&d)?;
        let path = d.join(format!("events.out.tfevents.{}.tbtui-demo.{}.0", t0 as u64, std::process::id()));
        let mut f = std::fs::File::create(&path)?;
        f.write_all(&events::frame_record(&events::encode_version_event(t0)))?;
        files.push(f);
    }
    let total = if live { i64::MAX } else { 3000 };
    let mut step = 0i64;
    println!("writing {} runs to {}{}", runs.len(), dir.display(), if live { " (Ctrl-C to stop)" } else { "" });
    while step < total {
        for (i, (_, speed, instab)) in runs.iter().enumerate() {
            let s = step as f64;
            let base = 2.3 * (-s * speed / 800.0).exp() + 0.15 + instab * (s / 300.0).sin();
            let loss = (base * (1.0 + 0.25 * noise())).max(1e-4) as f32;
            let val = (base * 1.08 + 0.05 + 0.02 * noise()) as f32;
            let acc = (1.0 - base / 2.5 + 0.02 * noise()).clamp(0.0, 1.0) as f32;
            let lr = (1e-3 * speed * (1.0 + (std::f64::consts::PI * s / 3000.0).cos()) / 2.0) as f32;
            let wall = if live { now() } else { t0 + s * 0.5 };
            let mut scalars = vec![
                ("loss/train", loss),
                ("lr", lr),
                ("grad_norm", (1.0 + 3.0 * noise().abs() / (1.0 + s / 500.0)) as f32),
            ];
            if step % 50 == 0 {
                scalars.push(("loss/val", val));
                scalars.push(("accuracy/val", acc));
            }
            files[i].write_all(&events::frame_record(&events::encode_scalar_event(wall, step, &scalars)))?;
        }
        step += 1;
        if live {
            for f in &mut files {
                f.flush()?;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    Ok(())
}
