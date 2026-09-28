# tbtui

TensorBoard scalars in your terminal. Point it at a log directory like you would
`tensorboard --logdir`, and get live, interactive, colored loss curves — or print
a one-off snapshot (great over SSH or in CI logs).

- Recursively finds `*tfevents*` files; one run per directory (TensorBoard semantics)
- Reads PyTorch / tensorboardX `simple_value` scalars and TF2 tensor scalars
- Live tailing: only new bytes are read on each refresh
- TensorBoard-style debiased EMA smoothing, log-y, outlier clipping, step or time x-axis
- Value cursor (keys or mouse hover), zoom/pan (keys or scroll wheel), grid view
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

## Keys

| key | action |
|---|---|
| `↑↓` `jk` | select tag (or run, after `Tab`) |
| `Tab` | switch focus tags ↔ runs |
| `Space` / click | toggle run; `a` all/none; `i` isolate run |
| `/` | filter tags (regex) |
| `g` | grid view of all filtered tags |
| `←→` `hl` / mouse hover | value cursor (legend shows values at that step); `Esc` clears |
| `+ -` / scroll wheel | zoom x; `[ ]` pan; `0` reset |
| `s` / `S` | more / less smoothing |
| `y` `o` `x` `u` | log-y · ignore outliers · step↔time · raw lines on/off |
| `r` `p` | reload now · pause live |
| `?` `q` | help · quit |

Colors use truecolor when `$COLORTERM` says so, 256 colors otherwise
(`--no-truecolor` forces 256). `NO_COLOR` / `--no-color` disables color in `snap`/`ls`.
