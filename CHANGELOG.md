# Changelog

All notable changes are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/) (before 1.0, minor versions may
change behavior; the notes below call this out).

## [Unreleased]

## [0.1.0] - 2026-09-29

First release.

### Added
- Interactive, live-updating terminal UI for TensorBoard scalars, and a one-shot
  `snapshot` mode plus `ls`, `demo`, `config`, `sessions` and `upgrade` commands.
- Reads PyTorch / tensorboardX scalars and TF2 tensor scalars from `*tfevents*`
  files found recursively, one run per directory; only appended bytes are read.
- Remote logs over ssh (`tbtui user@host:path`, or a named remote from
  `~/.config/tbtui/config.toml`) using the system `ssh`, with automatic reconnect.
- Collapsible trees for tags and runs, pinning several tags into an adaptive
  grid, resizable panes (keys or mouse), value cursor, zoom and pan.
- Regex filters for tags and runs, and grex-generated filters from examples.
- TensorBoard-style smoothing, log-y axis, outlier-ignoring y-scale, step or
  relative-time x-axis.
- Colorblind-safe palettes (Okabe-Ito default, Paul Tol bright) and a distinct
  marker shape per run.
- Sessions: the exact UI state is saved per set of logs and restored on restart
  (`tbtui -c` reopens the last one).
- `tbtui upgrade` checks crates.io and upgrades cargo installs.

[Unreleased]: https://github.com/paolo-moldovan/tbtui/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/paolo-moldovan/tbtui/releases/tag/v0.1.0
