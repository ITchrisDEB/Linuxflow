# LinuxFlow architecture

LinuxFlow is a Cargo workspace that produces **one binary** (`linuxflow`) running as a single process:
no local HTTP server and no separate engine service. This page describes the structure of the
project; the history of changes is in `CHANGELOG.md`.

## Diagram

```
                         apps/linuxflow  (binary, clap, gio::Application, wiring)
                                  |
          +-----------------------+------------------------+
          |                                                |
   linuxflow-gtk  (hub, pill, wizard,                linuxflow-platform
   shortcut recorder, CSS)                           traits + backends:
          |                                          hotkeys | inject | clipboard |
          |                                          focus | per_app | tray | autostart | open
          |                                                |
          +-------------+----------------+-----------------+
                        |                |                 |
               linuxflow-audio    linuxflow-stt     linuxflow-models
               cpal + rubato      whisper-rs        catalogue, download,
               level, VAD         (Vulkan / CPU)    resume, SHA-256
                        \                |                 /
                         +---------------+----------------+
                                         |
                                  linuxflow-core
        domain, traits, events, errors, constants (APP_ID...), XDG paths,
        settings, history, statistics, format/vocabulary, accelerators, PTT,
        dictation state machine, pipeline.  No GTK dependency: fully testable.

   linuxflow-i18n: gettext (English is the source language, catalogues in po/), used by the interface.
```

A `tokio` runtime runs in a separate thread (D-Bus portals, tray, downloads); the GLib main loop
stays on the main thread; inference has its own dedicated thread; channels connect everything.

## The crates

| Crate | Role |
|---|---|
| `linuxflow-core` | Pure logic: shared contracts (`domain`, `traits`, `events`, `error`, `constants`), `paths` (XDG and `LINUXFLOW_*` overrides), `store`/`settings`/`history`/`stats`, `format`/`vocab`, `accelerator`/`ptt`, `dictation`/`pipeline`. |
| `linuxflow-audio` | Capture (cpal), conversion and 16 kHz resampling (rubato), sound level, recorder. |
| `linuxflow-stt` | Speech recognition engine (whisper.cpp through whisper-rs), fallback chain selection, CPU guard, dynamic loading of Vulkan. `vulkan` feature. |
| `linuxflow-models` | Model catalogue, hardware-based recommendation, download with resume and SHA-256 verification. The only crate allowed to use the network. |
| `linuxflow-platform` | Session-dependent traits and backends: global shortcuts, paste injection, clipboard, active window, `PER_APP` table, tray, autostart, opening folders. Capability detection at run time. |
| `linuxflow-i18n` | Translation (`tr`, `tr_fmt`, `trn`), locale, formats, `.mo` catalogues. |
| `linuxflow-gtk` | Main window (hub), recording pill (layer-shell or fallback), first-launch wizard, models page. |
| `apps/linuxflow` | Binary: command line, GIO application, assembly. |

Shared dependencies live in the `[workspace.dependencies]` table of `Cargo.toml`; `Cargo.lock` is committed.

## Design rules

- **Detect, do not assume**: every capability (portal, layer-shell, data-control, Vulkan, tray) is
  probed at run time, with a fallback; no missing component prevents the application from starting.
- **No hard-coded local value**: paths come from XDG (`directories`) and can be overridden by
  `LINUXFLOW_CONFIG_DIR`, `LINUXFLOW_DATA_DIR`, `LINUXFLOW_STATE_DIR`, `LINUXFLOW_CACHE_DIR`,
  `LINUXFLOW_MODEL_DIR`; the application identifier is `constants::APP_ID`, its single source.
- **Network**: only the model download you asked for (`LINUXFLOW_OFFLINE`, `HF_HUB_OFFLINE`,
  `LINUXFLOW_MODEL_URL` / `HF_ENDPOINT` for a mirror). No telemetry, no local server.
- **Interface baseline**: GTK 4.12 / libadwaita 1.4 (`v4_12` / `v1_4` features only).
- **GPU**: Vulkan only (`vulkan` feature), CPU fallback; no CUDA or ROCm.
- **Visible text** goes through `linuxflow_i18n::tr("English msgid")` and is never built by concatenation.

## Data directories (defaults)

| Use | Directory |
|---|---|
| Settings, dictionary, snippets | `$XDG_CONFIG_HOME/linuxflow` (`~/.config/linuxflow`) |
| History | `$XDG_DATA_HOME/linuxflow` (`~/.local/share/linuxflow`) |
| Logs | `$XDG_STATE_HOME/linuxflow` (`~/.local/state/linuxflow`) |
| Cache, models | `$XDG_CACHE_HOME/linuxflow` (models in `models/`, or `LINUXFLOW_MODEL_DIR`) |

The installer also places static files under `<prefix>/share/linuxflow/` (manifest, copy of
`uninstall.sh`): this directory can therefore coincide with the user data directory when the prefix
is `~/.local`. The application must only write its own file names there.

## Distribution

Two forms: a `tar.gz` archive (`cargo xtask dist`) containing `bin/`, `share/`, `contrib/`
(optional udev rule), `install.sh`, `uninstall.sh`, `MIN_VERSIONS` and `SHA256SUMS`, and an AppImage
(`cargo xtask appimage`) that bundles GTK 4 and libadwaita. `cargo xtask install` builds the
archive and installs it. See `BUILD.md`. Checks: `cargo xtask check`, `cargo xtask static-checks`
(T-HARD, T-ID, T-PRIV, T-NET, T-LIC), `scripts/tests/test-packaging.sh`.

## Adding...

- **An interface language**: add `po/<language>.po` (template: `po/linuxflow.pot`, tool:
  `scripts/update-po.sh`). `cargo xtask dist` compiles each `po/<language>.po` into
  `share/locale/<language>/LC_MESSAGES/linuxflow.mo`; files whose name contains a dot
  (`fr.compendium.po`) are ignored.
- **A platform backend**: implement the matching trait (`HotkeyBackend`, `Injector`,
  `ClipboardBackend`, `ActiveWindow`) in `linuxflow-platform`, add it to the fallback chain of the
  capabilities module, and cover it with tests that use simulated capabilities.
- **A formatting rule**: in `linuxflow-core/src/format.rs`, with a case in
  `crates/linuxflow-core/tests/golden/`; language-dependent rules follow the language of the text,
  never the language of the interface.
- **An icon or a shipped data file**: icons through `scripts/gen-icons.sh`; any file placed under
  `packaging/share/` is copied as is into `share/` of the archive.

## Rust everywhere, except the graphical interface

All the application and tooling code is Rust. The exceptions, and why:

| Item | Language | Reason |
|---|---|---|
| Interface (GTK 4, libadwaita, layer-shell) | C, through the Rust bindings | project choice: the interface is the only non-Rust part; the code that drives it is Rust, and the stylesheet is GTK CSS |
| Speech engine (whisper.cpp / ggml) | C++, through `whisper-rs` | no pure-Rust engine offers Vulkan GPU acceleration (research: `oxiwhisper` and `whisper-apr` are CPU/WASM only) |
| `install.sh`, `uninstall.sh` | POSIX shell | they run before any binary is installed |
| `scripts/update-po.sh` | bash + GNU gettext | maintenance of the translation catalogues (development only); compiling `.po` to `.mo` is done in Rust |
| `scripts/gen-icons.sh` | shell | one-off generation of the icon files (committed) |
| `xdg-open` | external program | last resort only, when no portal is available; the OpenURI portal (Rust) is tried first |

Building the archive, generating the notices and licence texts, the static checks and the quality
gate are all in Rust: `cargo xtask <command>` (the `xtask/` directory, a separate workspace whose
dependencies never enter the shipped binary). `hyprctl` is no longer called: pasting under Hyprland
talks directly to its command socket.
