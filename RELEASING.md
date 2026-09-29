# Releasing

One-time setup: create a crates.io API token (crates.io → Account Settings → API
Tokens, scope "publish-update", plus "publish-new" for the first release) and
store it as the GitHub repository secret `CARGO_REGISTRY_TOKEN`.

For each release:

1. Move the `[Unreleased]` notes in `CHANGELOG.md` under a new `## [X.Y.Z] - date`
   heading and update the compare links at the bottom.
2. Bump `version` in `Cargo.toml`, then run `cargo build --locked` (or `cargo check`)
   so `Cargo.lock` follows.
3. Check everything locally:
   ```bash
   cargo fmt --check && cargo clippy --all-targets --locked -- -D warnings && cargo test --locked
   cargo publish --dry-run --locked
   ```
4. Commit, tag, push:
   ```bash
   git commit -am "Release vX.Y.Z"
   git tag vX.Y.Z
   git push origin main vX.Y.Z
   ```
5. The `release` workflow checks that the tag, `Cargo.toml` and `CHANGELOG.md`
   agree, builds the Linux and macOS binaries, creates the GitHub release, and
   runs `cargo publish`. Users then get it with `tbtui upgrade`,
   `cargo install tbtui`, or `cargo binstall tbtui`.

Publishing to crates.io cannot be undone (a bad release can only be `cargo yank`ed),
so run step 3 first. To publish by hand instead of via the workflow:
`cargo login` once, then `cargo publish --locked`.

## Compatibility promises

- Saved sessions (`~/.local/state/tbtui/sessions`) ignore unknown fields and fill
  in missing ones, so sessions survive upgrades and downgrades.
- `config.toml` rejects unknown keys on purpose (to catch typos). Adding a key
  in a release is therefore backward-compatible, but an older tbtui will refuse
  a config that uses it. Mention such keys in the changelog.
