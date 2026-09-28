# tbtui

TensorBoard scalars in your terminal. Point it at a log directory like you would
`tensorboard --logdir`, and get live, interactive, colored loss curves — or print
a one-off snapshot (great over SSH or in CI logs).

- Recursively finds `*tfevents*` files; one run per directory (TensorBoard semantics)
- Reads PyTorch / tensorboardX `simple_value` scalars and TF2 tensor scalars
- Live tailing: only new bytes are read on each refresh
- TensorBoard-style debiased EMA smoothing, log-y, outlier clipping, step or time x-axis
- Collapsible trees for tags and runs (split on `/`; shared prefixes merge into one row)
- Pin any set of tags to see them side by side; the grid adapts to the window and pane sizes
- Resizable panes (drag the borders or use `<` `>` `{` `}`), adjustable grid columns
- Regex filters for tags and runs, or build one from examples with [grex](https://github.com/pemistahl/grex)
- Colorblind-safe palettes (Okabe-Ito by default, Paul Tol bright) plus a shape per run drawn on the lines
- Value cursor (keys or mouse hover), zoom/pan (keys or scroll wheel)
- Single ~2 MB static binary, no Python, no protobuf codegen. 1.5M points load in ~0.1 s

## Install

Needs a Rust toolchain (`curl https://sh.rustup.rs -sSf | sh`):

```bash
cargo install --path .
```

Prebuilt Linux (musl, x86_64/arm64) and macOS binaries: push a `v*` tag and
`.github/workflows/release.yml` attaches them to a GitHub release.

## Use

```bash
tbtui runs/                      # interactive, live-updating (like tensorboard --logdir runs/)
tbtui runs/ -f 'loss' -s 0.9     # start filtered to loss tags, heavier smoothing
tbtui runs/ -O                   # ignore outliers: y-axis fits the 5th–95th percentile (toggle: o)
tbtui exp_a/ exp_b/              # several roots; runs get prefixed with the root name
tbtui snap runs/ -t loss         # print charts once and exit
tbtui snap runs/ -t 'val' -c 3 -H 14 --log-y
tbtui ls runs/                   # table of runs/tags with latest values
tbtui demo /tmp/demo --live      # fake training job to try it out
```

## Remote logs over ssh

Same command, scp-style target:

```bash
tbtui paolo@10.0.0.5:~/runs              # ad-hoc
tbtui gpu1:~/runs                        # Host alias from ~/.ssh/config
tbtui -p 2222 -i ~/.ssh/lab_key -J bastion paolo@10.0.0.5:/data/tb
tbtui gpu1                               # named remote from the config file (uses its `path`)
tbtui snap gpu1 -t loss                  # snap / ls work the same way
tbtui gpu1 ./local_runs                  # mix remote and local; runs are prefixed by source
```

Named remotes live in `~/.config/tbtui/config.toml` (or `$TBTUI_CONFIG`, or `--config FILE`).
`tbtui config --init` writes a commented example; `tbtui config` lists what's defined.

```toml
[remotes.gpu1]
host = "10.0.0.5"                 # IP / hostname / ~/.ssh/config alias (default: the name)
user = "paolo"
port = 22
path = "~/experiments/runs"       # default log dir on that host
identity_file = "~/.ssh/id_ed25519"
ssh_config = "~/.ssh/config"      # ssh -F
jump = "paolo@bastion"            # ssh -J
ssh_args = ["-o", "StrictHostKeyChecking=accept-new"]
```

Command-line flags (`-l/--user`, `-p/--port`, `-i/--identity`, `-F/--ssh-config`,
`-J/--jump`) override the config.

How it works: tbtui runs your system `ssh`, so keys, the agent, `~/.ssh/config`
and ProxyJump behave exactly as in a normal ssh session. It opens one shared
connection when it starts. This is where a password, passphrase or host-key
prompt can appear, before the UI opens. The connection stays open for
10 minutes after exit, so re-launching is instant. Each refresh runs a small
POSIX `sh` script over that connection, which sends only the bytes appended to
each event file since the last poll. Nothing needs to be installed on the
server. If the connection drops, tbtui reconnects on its own and shows the
error in the top bar until it succeeds.

## Sessions

tbtui remembers exactly how you left it:

- which logs are open, including remote targets and their ssh settings
- pane sizes and folded groups
- tree cursor and scroll positions
- filters, including grex examples and options
- pinned tags and hidden runs
- zoom, value cursor and focused chart
- smoothing, view options, palette, and run colors

It saves on quit, and within about 2 s of any change, so a dropped ssh
session or a crash loses nothing.

```bash
tbtui runs/          # same logs as before → same view as before
tbtui -c             # reopen the last session, from any directory
tbtui sessions       # list saved sessions, newest first
tbtui runs/ --fresh  # start clean (the saved state is replaced on exit)
tbtui runs/ -s 0.9   # options typed on the command line win over the saved state
```

Sessions are stored per set of log targets in `~/.local/state/tbtui/sessions/`
(or `$XDG_STATE_HOME/tbtui`).

## Keys

Press `?` in the app for the full list.

| key | action |
|---|---|
| `Tab` | focus tags → runs → charts |
| `↑↓` `←→` `Enter` / click arrow | move, fold/unfold tree groups · `C` fold all |
| `Space` / click mark | tags: pin (a group pins all its tags) · runs: show/hide |
| `a` · `i` | pin all/none, show all/none · isolate run(s) |
| `g` | grid of all (filtered) tags |
| `/` | filter the focused tree (regex) |
| `e` | filter by example: starts with the highlighted item |
| `<` `>` · `{` `}` · `,` `.` `;` | sidebar width · tags/runs split · grid columns (`;` = auto) · or drag borders |
| `b` | hide sidebar |
| charts: `←→` `↑↓` / hover | value cursor · focused chart |
| `+ -` / wheel · `[ ]` · `0` | zoom · pan · reset |
| `s` `S` · `y` `o` `x` `u` `m` | smoothing · log-y, outliers, step↔time, raw lines, shape markers |
| `P` | palette: okabe-ito → tol-bright → vivid |
| `r` `p` · `q` | reload · pause · quit |

What the charts show: every tag with `g`; otherwise the pinned tags; with
nothing pinned, the highlighted tag, or all tags of a highlighted group.

### Filtering by example (grex)

In the filter editor (`/` or `e`), `Tab` or a click adds or removes the
highlighted item as an example. `^T` switches to typing an example yourself.
grex turns the examples into a regex that is applied live. While the editor is
open, non-matching items stay in the tree (dimmed) so you can still pick them.

Options:

- `^D` turns digits into `\d`, so `seed0` and `seed1` also match `seed7`.
- `^W` turns word characters into `\w`.
- `^R` collapses repeated parts.
- `^A` switches between matching the whole name and matching anywhere in it.

You can also edit the regex by hand. `Enter` keeps the filter, `Esc` restores
the previous one.

Colors use truecolor when `$COLORTERM` says so, 256 colors otherwise
(`--no-truecolor` forces 256). Choose the run palette with `--palette` or
`palette = "tol-bright"` in the config file. `NO_COLOR` / `--no-color` disables color in `snap`/`ls`.
