# Changelog

Format inspired by [Keep a Changelog](https://keepachangelog.com/); semantic versioning.

## [0.2.0] — light and dark theme

### Added
- **Light and dark theme** for the whole interface (hub, wizard, dialogs). By default LinuxFlow
  follows the light or dark preference of the desktop, live, through libadwaita (XDG appearance
  portal / GSettings); an **Appearance** setting (System / Light / Dark) in the settings and on the
  first step of the wizard forces one or the other.

### Changed
- The colours of the interface are now named and defined by two palettes (light and dark) instead of
  being fixed to the light theme; the shortcut highlight on the home page works in both themes.

## [0.1.0] — first release candidate

Local voice dictation for Linux, written in Rust (GTK 4 / libadwaita), distributed as a `tar.gz`
archive and as an AppImage.

### Added
- **Application**: main window (home, history, dictionary, snippets, models, settings,
  troubleshooting, first-launch wizard), recording pill, tray icon with a menu, single instance,
  command-line control (`--toggle`, `--start`, `--stop`, `--cancel`, `--page`, `--quit`,
  `--background`, `--test-pipeline`…), interface in **English and French** (the language can be
  switched live, and is offered on the first step of the wizard).
- **Local speech recognition**: Whisper through whisper.cpp in an isolated child process
  (automatic restart), Vulkan GPU with a CPU fallback, a fallback engine for CPUs without AVX2,
  and a speech filter (silence produces no invented text).
- **Models**: catalogue, hardware-based recommendation, download with resume and SHA-256
  verification, offline mode, import of a local file, integrity check. The recommended model is
  preselected in the wizard; the helper VAD model is downloaded automatically with the first model.
- **Text formatting and vocabulary** verified on thousands of reference cases; two deliberate
  choices: a snippet expansion is inserted as is, and vocabulary words can no longer be altered
  by digits.
- **Platform**: global shortcut, push-to-talk, pasting (X11, RemoteDesktop portal, Wayland virtual
  keyboard, Hyprland, optional uinput), clipboard, active window (X11, Sway, Hyprland), launch at
  login, and capability detection with a fallback for every missing component.
- **Weak microphone** setting (automatic by default), applied before transcription.
- **Distribution**: reproducible archive, `install.sh` / `uninstall.sh`, full texts of the
  third-party licences (`THIRD_PARTY_LICENSES.txt`), bilingual `.desktop` file with actions.

### Shortcuts
- **The shortcut is configured from inside the application**, on every desktop: X11 key grabs
  (Xfce, Cinnamon, MATE, KDE and GNOME on X11), the KDE global shortcut service, a GNOME custom
  shortcut, Sway and Hyprland bindings, with the XDG GlobalShortcuts portal and optional evdev as
  fallbacks. When a key is already taken, the application names the program that uses it.
- Combinations that the system or nearly every application uses are refused (Ctrl+C/V/X…, Alt+Tab,
  Alt+F4, Ctrl+Alt+Delete, Ctrl+Alt+F1–F12, Super+L).
- The numpad keys (digits and operators) can be part of a shortcut.
- When the desktop assigns a different key than the requested one, the key actually assigned is
  shown; a shortcut with no assigned key is reported as unavailable.

### Interface
- The **Troubleshooting** page (below Settings) groups the folder shortcuts (logs, data, models),
  the speech engine restart and the compatibility report.
- The tray icon is the application logo; its bars turn **green** while dictating. It never blinks.
- A lost paste permission (the desktop can revoke the remote-control session at any time) is
  re-opened automatically and the paste is retried once.

### AppImage
- `cargo xtask appimage` builds `LinuxFlow-<version>-x86_64.AppImage`: GTK 4.14, libadwaita 1.5 and
  their dependencies (official Ubuntu 24.04 packages, SHA-256 verified) are bundled; glibc, the
  graphics drivers and audio stay those of the system. Launch at login targets the `.AppImage` file.

### Distribution compatibility
- `libgtk4-layer-shell` (missing from Ubuntu 24.04 LTS, RHEL 10 and others) is no longer a
  dependency of the application: the floating layer-shell pill lives in a separate program,
  `linuxflow-overlay`. When the library is missing the application still starts and the pill goes
  through XWayland (as on GNOME).
- `DEPENDENCIES.md`: run-time and build dependencies, with the versions shipped by current and
  recent distributions (Ubuntu, Debian, Fedora, RHEL and derivatives, openSUSE, Arch and
  derivatives, NixOS, etc.).

### Tooling in Rust
- `cargo xtask dist | appimage | install | notices | licenses | static-checks | check` replace the
  shell scripts for building and checking. `cargo xtask install` is the equivalent of
  `make && make install`.
- Local build paths are removed from the shipped binaries, including the C/C++ code of whisper.cpp.
- Pasting under Hyprland talks directly to its command socket (no `hyprctl` program).
- The log traces each dictation (start, audio duration, words, timings, destination), never the text.

### Robustness
- A late result of a cancelled transcription can no longer corrupt the next dictation.
- An out-of-range or mistyped setting no longer blocks all settings or resets them; an unreadable
  file is saved as `.corrupt` before being replaced.
- Opening folders is limited to an allow-list (no arbitrary path).
- No local HTTP server: no port is opened.

### Known limits
- Tested on KDE Plasma under Wayland, x86-64, only. GNOME, Sway, Hyprland and X11 are covered by
  automated tests with simulated desktops but **not verified on real sessions** (checklist:
  `docs/manual-validation.md`). The GNOME, Sway and Hyprland shortcuts only toggle dictation
  (no hold-to-talk).
- Vulkan GPU acceleration was tested on a single card (a recent NVIDIA discrete GPU). The GPU engine is a
  separate executable linked to `libvulkan.so.1`: the application and the CPU engine start without Vulkan.
- Only text is restored to the clipboard after a dictation.
- No noise suppression (measured: it degrades transcription). A saturated (too loud) signal cannot
  be repaired.
