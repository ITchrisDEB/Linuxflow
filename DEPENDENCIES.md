# Dependencies

What LinuxFlow needs to **run**, what it can use if present, and what you need to **build** it,
distribution by distribution. The installer (`install.sh`) checks the required libraries and prints
the exact command for your distribution when one is missing; it never installs packages itself
(that needs `sudo`, and it is your decision).

LinuxFlow needs **no** Python, Node.js, Java, Docker, CUDA or ROCm, and it runs no local server.

**Wayland first.** Most current distributions start a Wayland session by default (GNOME, KDE Plasma 6,
COSMIC, Hyprland, Sway), and some have dropped the X11 session altogether (GNOME 49 on Ubuntu 25.10
and Fedora 43). Wayland is therefore the main target, with X11 supported too. What differs on Wayland
is listed in section 3 and in the README ("Shortcuts depend on your desktop").

## 1. Which distributions can run the prebuilt archive

The archive needs **GTK >= 4.12**, **libadwaita >= 1.4** and **glibc >= 2.39** (`packaging/MIN_VERSIONS`),
on x86-64 with AVX2 (see section 2). Versions below come from the distributions' package repositories
(Repology, checked on 2026-10-25). The last column tells whether the optional layer-shell package
exists there (section 3).

| Distribution and release | GTK 4 | libadwaita | glibc | Prebuilt archive | gtk4-layer-shell in repo |
|---|---|---|---|---|---|
| **Ubuntu 22.04 LTS** (and Mint 21, Pop!_OS 22.04, Zorin 17) | 4.6 | 1.1 | 2.35 | **no**: build from source | no |
| **Ubuntu 24.04 LTS** (and Mint 22, Pop!_OS 24.04, Zorin 18, elementary 8) | 4.14 | 1.5 | 2.39 | **yes** | no |
| **Ubuntu 25.04 / 25.10** | 4.18 / 4.20 | 1.7 / 1.8 | 2.41 / 2.42 | **yes** | yes |
| Ubuntu 26.04 and later | newer | newer | newer | expected yes (not checked) | yes |
| **Debian 12** (bookworm) | 4.8 | 1.2 | 2.36 | **no**: build from source | no |
| **Debian 13** (trixie), Raspberry Pi OS (Debian 13) | 4.18 | 1.7 | 2.41 | **yes** | yes |
| Debian testing / unstable, Kali rolling | 4.24 | 1.10 | 2.43 | **yes** | yes |
| Fedora 39 | 4.12 | 1.4 | 2.38 | **no**: glibc too old | yes |
| **Fedora 40, 41, 42, 43, 44** | 4.14 to 4.22 | 1.5 to 1.9 | 2.39 to 2.43 | **yes** | yes |
| **RHEL, AlmaLinux, Rocky, CentOS Stream 9** | 4.12 | 1.4 | 2.34 | **no**: glibc too old | no |
| **RHEL, AlmaLinux, Rocky, CentOS Stream 10** | 4.16 | 1.6 | 2.39 | **yes** | no |
| **openSUSE Leap 15.6** | 4.12 | 1.4 | 2.38 | **no**: glibc too old | no |
| **openSUSE Tumbleweed** | 4.22 | 1.9 | 2.44 | **yes** | yes |
| **Arch Linux**, EndeavourOS, CachyOS | 4.24 | 1.10 | 2.44 | **yes** | yes |
| **Manjaro** (stable) | 4.22 | 1.9 | 2.44 | **yes** | yes |
| NixOS 24.05 / 24.11 / 25.05 / 25.11 | 4.14 to 4.20 | 1.5 to 1.8 | 2.39 to 2.40 | **yes** | yes |
| Gentoo, Solus, Void (glibc build) | 4.22 to 4.24 | 1.9 to 1.10 | 2.41 to 2.44 | **yes** | yes |
| Mageia 9 / 10 | 4.10 / 4.20 | 1.3 / 1.8 | 2.36 / 2.42 | no / **yes** | no |
| Alpine (3.19 to 3.24), Void (musl) | 4.12 to 4.22 | 1.4 to 1.9 | musl | **no**: needs glibc, build from source | partly |
| Slackware 15.0 | 4.4 | none | 2.33 | **no**: build from source | no |

Reading the table:

- **"no: build from source"** means the prebuilt binary will not start (glibc or GTK too old, or
  musl instead of glibc). Everything still builds from source as soon as GTK >= 4.12 and libadwaita
  >= 1.4 can be installed (`BUILD.md`); this setup is unsupported but possible. On a glibc that is too
  old, upgrade the distribution rather than the library.
- Derivatives follow their base (Linux Mint, Pop!_OS, Zorin, elementary: Ubuntu; Raspberry Pi OS, Kali: Debian;
  EndeavourOS, CachyOS, Manjaro: Arch).
- Releases newer than those listed ship the same or higher versions: nothing in the requirements is
  removed upstream.
- 64-bit ARM (aarch64) is not covered by the prebuilt archive; building from source is untested.

### Required libraries (runtime)

| Library | Minimum | Debian / Ubuntu | Fedora / RHEL family | Arch | openSUSE |
|---|---|---|---|---|---|
| GTK 4 (`libgtk-4.so.1`) | 4.12 | `libgtk-4-1` | `gtk4` | `gtk4` | `libgtk-4-1` |
| libadwaita (`libadwaita-1.so.0`) | 1.4 | `libadwaita-1-0` | `libadwaita` | `libadwaita` | `libadwaita-1-0` |
| ALSA library (`libasound.so.2`) | any | `libasound2t64` (`libasound2` before 24.04) | `alsa-lib` | `alsa-lib` | `libasound2` |
| glibc | 2.39 | installed | installed | installed | installed |
| C++ runtime (`libstdc++`, `libgcc_s`) | any | installed | installed | installed | installed |

GTK 4 itself pulls in Pango, Cairo, GLib, Graphene and the usual desktop libraries. Package names
change between releases; search by library name when one is wrong: `apt-file search libgtk-4.so.1`,
`dnf provides '*/libgtk-4.so.1'`, `pacman -F libgtk-4.so.1`, `zypper search --provides libgtk-4.so.1`.

## 2. Hardware

- **CPU**: x86-64 with AVX2 (Intel since Haswell, 2013; AMD since Excavator / Zen). Older x86-64 CPUs
  use the bundled fallback engine `linuxflow-engine-v2`, about 6.5 times slower.
- **GPU** (optional): any GPU with a working Vulkan driver, Vulkan 1.2 or newer recommended (NVIDIA proprietary, Mesa RADV or ANV).
  Without one, LinuxFlow runs on the CPU with the lighter model and says so in the Compatibility page.
- **Disk and memory**: about 25 MB for the application; a speech model takes 0.5 to 3.1 GB on disk
  (downloaded at first launch, section 5) and 1 to 3.5 GB of RAM or VRAM while in use
  (`docs/measurements.md`).

## 3. Runtime: optional services, and what changes on Wayland

| Feature | Needs | Without it |
|---|---|---|
| **Floating recording pill (Wayland)** | the compositor's layer-shell **and** the `libgtk4-layer-shell` package (KDE Plasma, Sway, Hyprland, COSMIC) | **GNOME** has no layer-shell: the pill is shown through XWayland. If the package is missing (Ubuntu 24.04, RHEL 10...) LinuxFlow starts normally and uses the same XWayland fallback. Package names: Debian/Ubuntu/openSUSE `libgtk4-layer-shell0`, Fedora/Arch `gtk4-layer-shell` (the Arch and openSUSE names were not checked) |
| **Global shortcut (Wayland)** | `xdg-desktop-portal` + the backend of your desktop (`-gnome`, `-kde`, `-gtk`, `-wlr`, `-hyprland`) with the GlobalShortcuts portal | bind your desktop's own shortcut to `linuxflow --toggle` |
| **Automatic paste (Wayland)** | RemoteDesktop portal, or the virtual-keyboard protocol (wlroots, KDE), or Hyprland (native socket) | the text stays in the clipboard: paste it yourself |
| Global shortcut and paste (X11) | nothing to install (XGrabKey, XTEST) | |
| Clipboard | portal, Wayland data-control, X11 selections, or GTK's own clipboard (last resort) | one of them is always tried |
| Audio capture | PipeWire (with `pipewire-alsa`, the default on every distribution above), or PulseAudio, or plain ALSA | the microphone list is empty and dictation reports a clear error |
| Tray icon | a StatusNotifier host: built into KDE Plasma, Xfce, Cinnamon, MATE, LXQt, COSMIC; **GNOME needs the "AppIndicator and KStatusNotifierItem Support" extension** | no tray icon; the main window and the command line still work |
| GPU speech recognition | the Vulkan loader (`libvulkan1` / `vulkan-loader` / `vulkan-icd-loader`) and a driver | CPU |
| Hold-to-talk on Wayland, key simulation without a portal | membership of the `input` group and/or the uinput udev rule `contrib/99-linuxflow-uinput.rules` | off by default; both have security implications (see the README) |
| Opening folders and links | OpenURI portal, then `xdg-open` as a last resort | the folder is not opened |

The **Compatibility** page of the application shows the state of each of these on your machine and
how to fix what is missing.

## 4. What the installer does about dependencies

- Reads the libraries each binary needs (`ldd`) and the versions of GTK, libadwaita and glibc.
- **Required library missing, or version below the minimum**: stops before writing anything and prints
  the install command for your distribution family (not executed), or the lookup command when it does
  not know the package name.
- **Optional library missing** (Vulkan loader, layer-shell): continues with a warning that says what
  changes.
- Does **not** install packages. It writes under `~/.local` by default. Run as root (`sudo ./install.sh`) it
  installs for all users under `/usr/local`; `--prefix DIR` chooses another place.

## 5. Downloaded at first launch (not at install time)

Only with your confirmation, from `huggingface.co` (or a mirror set with `LINUXFLOW_MODEL_URL`, or a
local file, or offline with `LINUXFLOW_OFFLINE=1`), into `~/.cache/linuxflow/models`:

| File | Size | Use |
|---|---|---|
| `ggml-small.bin` | 488 MB | CPU, or no usable GPU |
| `ggml-large-v3-q5_0.bin` | 1.08 GB | GPU, recommended |
| `ggml-large-v3.bin` | 3.1 GB | GPU, most accurate |
| `ggml-silero-v6.2.0.bin` | 1 MB | speech detector (silence is not transcribed) |

Every file is checked against a SHA-256 sum. Only one Whisper model is used at a time.

## 6. Shipped inside the archive (nothing to install)

- `linuxflow` (the application), `linuxflow-overlay` (the layer-shell pill, the only program that needs
  `libgtk4-layer-shell`), and three engines: `linuxflow-engine` (CPU), `linuxflow-engine-v2` (CPU without
  AVX2), `linuxflow-engine-vulkan` (GPU, the only one linked to `libvulkan.so.1`).
- whisper.cpp / ggml (inside the engines) and all Rust libraries, linked statically (list and licences:
  `THIRD_PARTY_NOTICES.md`, `share/doc/linuxflow/THIRD_PARTY_LICENSES.txt`).
- The Figtree font (SIL Open Font License), icons, French and English translations.

## 7. Testing without installing: the AppImage

`LinuxFlow-<version>-x86_64.AppImage` (about 60 MB) runs without any installation: `chmod +x` it and
double-click or run it. It carries GTK 4.14, libadwaita 1.5 and 117 other libraries taken from the official
**Ubuntu 24.04** packages (checked by SHA-256), so a distribution without GTK 4.12 can run it.

What it still takes from the system: the **glibc (2.39 or newer, the same as the archive)**, the graphics
drivers (OpenGL, Vulkan loader), ALSA/PipeWire for the microphone, and the fonts. Distributions older
than glibc 2.39 (Ubuntu 22.04, Debian 12, RHEL 9...) cannot run it either. Needs `libfuse2` to mount itself;
without FUSE, run it with `--appimage-extract-and-run`. The "launch at login" setting writes the path of the
`.AppImage` file, so keep the file where it is.

## 8. Build-time dependencies (only if you compile it yourself)

See `BUILD.md` for the full commands per distribution. In short:

- Rust 1.92 or newer, a C and C++ compiler, `cmake`, `pkg-config`, `libclang`.
- Development files for GTK 4 (>= 4.12), libadwaita (>= 1.4), ALSA, xkbcommon (and its X11 part) and,
  for the layer-shell pill, gtk4-layer-shell (without it, build with the `linuxflow-overlay` program
  skipped: the pill then uses XWayland).
- For the GPU engine: the Vulkan headers and `glslc` (otherwise `cargo xtask dist --no-vulkan`).
- To maintain translations only: GNU gettext (`xgettext`, `msgmerge`).
- The AppImage (`cargo xtask appimage`): `curl`, `glib-compile-schemas`, network access to Ubuntu's archive and
  GitHub (appimagetool, its runtime).
- Optional development tools: `cargo-deny`, `cargo-llvm-cov` (with rustup's `llvm-tools` component), `shellcheck`.

Building the archive, the licence texts and the quality checks is done by `cargo xtask`, written in Rust.
