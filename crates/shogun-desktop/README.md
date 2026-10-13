# shogun-desktop

The shogun-desktop app for the old multi-agent-shogun layout
(`queue/*.yaml`, `dashboard.md`, fixed tmux panes). The root app is now
honden-desktop; this crate keeps the old behaviour available.

It is a copy of the root app at master
`ac4308961f7e7a2b972ebb1d7c63753809d5a78b`, not a move. Fixes made to the
root after that commit do not reach this copy unless ported by hand.

```
cargo build --release -p shogun-desktop
# -> target/release/shogun-desktop(.exe)
```

- **Names.** The package and main bin are `shogun-desktop`; the root is
  `honden-desktop`. Only the diagnostic bin stays `fontprobe-legacy`, because
  the root keeps its own `fontprobe` and same-named bins would overwrite each
  other in `target/<profile>/`.
- **Assets.** The bundled fonts are read from the root `assets/fonts/` so the
  ~9.7 MB of TTFs is not stored twice. Only `assets/icon.ico` is copied, so a
  later icon change at the root does not reach this build.
- **Settings and credentials.** The settings dir (`<config>/shogun-desktop/`),
  the keyring service name (`shogun-desktop`), the log name and the macOS
  notification bundle id are unchanged. Until the root app moves to its own
  honden locations, this copy and the root app share the same settings and
  stored SSH password on one machine.
