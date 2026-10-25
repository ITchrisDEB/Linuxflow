# Building LinuxFlow from source

Use this for distributions below the supported base (GTK >= 4.12, libadwaita >= 1.4) or to
hack on the code. A source build on an older system is **unsupported** unless you can provide
GTK >= 4.12 and libadwaita >= 1.4 (jhbuild, your own prefix...). The workspace enables the
`v4_12` / `v1_4` features only, never higher.

Status: the whole workspace builds and its tests pass on the development machine (Arch-based,
GTK 4.24, libadwaita 1.10, glibc 2.44). The release archive builds with
`cargo xtask dist` (several minutes: whisper.cpp is compiled three times from source: the
CPU engine, the fallback engine for CPUs without AVX2, and the Vulkan engine). The GPU engine was
checked on a recent NVIDIA discrete GPU (real transcription with `large-v3` q5_0, detection of both the
NVIDIA and the AMD integrated GPU); other GPUs and drivers have not been tried.

## Prerequisites

- Rust >= 1.92 (`rust-version` in `Cargo.toml`), from <https://rustup.rs> or your distribution.
- A C and C++ compiler, `cmake`, `pkg-config`, `make` or `ninja`, and `libclang` (the
  `whisper-rs` bindings are generated with bindgen).
- Development files for GTK 4 (>= 4.12), libadwaita (>= 1.4), ALSA and xkbcommon (+ x11).
- `gtk4-layer-shell` development files, **only for the floating-pill program** `linuxflow-overlay`
  (it is a separate program because many distributions do not package this library; without it, skip
  `-p linuxflow-overlay`: the application still works and shows the pill through XWayland).
- GPU engine (built by `cargo xtask dist`): Vulkan headers and `glslc` (shader compiler). Without
  them, build a CPU-only archive with `--no-vulkan`.
- `gettext` (`xgettext`, `msgmerge`) only to maintain the translation catalogues (`scripts/update-po.sh`);
  building the archive compiles the `.po` files itself, in Rust.

Package names (indicative, check your release):

| Family | Command |
|---|---|
| Debian / Ubuntu | `sudo apt install build-essential cmake pkg-config libclang-dev libgtk-4-dev libadwaita-1-dev libasound2-dev libxkbcommon-dev libxkbcommon-x11-dev libgtk4-layer-shell-dev libvulkan-dev glslc gettext` |
| Fedora | `sudo dnf install gcc gcc-c++ cmake pkgconf-pkg-config clang-devel gtk4-devel libadwaita-devel alsa-lib-devel libxkbcommon-devel libxkbcommon-x11-devel gtk4-layer-shell-devel vulkan-headers vulkan-loader-devel glslc gettext` |
| Arch | `sudo pacman -S base-devel cmake clang gtk4 libadwaita alsa-lib libxkbcommon libxkbcommon-x11 gtk4-layer-shell vulkan-headers vulkan-icd-loader shaderc gettext` |
| openSUSE | `sudo zypper install gcc gcc-c++ cmake pkg-config clang-devel gtk4-devel libadwaita-devel alsa-devel libxkbcommon-devel libxkbcommon-x11-devel gtk4-layer-shell-devel vulkan-headers vulkan-devel shaderc gettext-tools` |

These commands are printed for convenience and were not run by the project tooling. Some
distributions do not ship `gtk4-layer-shell` development files, and package names change between
releases: whenever a name is wrong, search for the `.pc` file or library named in the build error.

## Build and test

```sh
cargo build --release --locked -p linuxflow -p linuxflow-engine   # application + CPU engine
cargo build --release --locked -p linuxflow-overlay                  # layer-shell pill (needs libgtk4-layer-shell)
cargo build --release --locked -p linuxflow-engine --features vulkan   # GPU engine (needs the Vulkan headers and glslc)
cargo test --workspace --locked
cargo xtask check                           # fmt, clippy -D warnings, tests, licences, static checks
```

Do not build distributed binaries with `-C target-cpu=native`: whisper.cpp's CPU level is fixed
at compile time, so a native build may not start on another machine. `cargo xtask dist`
refuses it and sets `GGML_NATIVE=OFF` (override `GGML_*` explicitly if you know what you do).

## Install from your build

One command, the equivalent of `make && make install` (it builds the release archive, extracts it
and runs its `install.sh`, so it exercises the same code path as a published release):

```sh
cargo xtask install                         # installs for your user under ~/.local
cargo xtask install --system                # builds as you, installs under /usr/local with sudo
cargo xtask install -- --prefix /opt/linuxflow --dry-run   # options after `--` go to install.sh
```

Or do it in two steps:

```sh
cargo xtask dist                            # writes target/dist/linuxflow-<version>-<arch>.tar.gz
tar -xzf target/dist/linuxflow-*.tar.gz -C /tmp/lf && cd /tmp/lf/linuxflow-*
./install.sh --prefix "$HOME/.local"
```

`cargo xtask dist` options: `--fake-binary FILE` (package an existing file without compiling,
useful to test the installer), `--out DIR`, `--version V`, `--arch A`, `--features F`, `--no-vulkan` (CPU-only archive). The archive
contains three engines: `linuxflow-engine` (CPU, starts everywhere), `linuxflow-engine-v2` (CPUs
without AVX2) and `linuxflow-engine-vulkan` (linked against `libvulkan.so.1`, so it is only started when
a Vulkan GPU has actually been detected; the application and the CPU engines do not need Vulkan). The archive
is reproducible: the file order, owner and modification times are normalised (`SOURCE_DATE_EPOCH`,
by default the date of the last commit), with GNU tar and `gzip -n`. Local build paths are
remapped out of the binary. Two builds of the packaging step with the same inputs give the same
SHA-256 (verified with a fake binary; a full two-build comparison of the compiled binary has not
been done).

## Oldest-glibc builds

A binary only runs on glibc versions at least as recent as the one it was built against. For a
release archive, build on the oldest supported distribution (a VM or chroot, no Docker), or use
`cargo-zigbuild --target x86_64-unknown-linux-gnu.<glibc>`. Then record the real requirement
(`objdump -T bin/linuxflow | grep -o 'GLIBC_[0-9.]*' | sort -Vu | tail -1`) in
`packaging/MIN_VERSIONS` (`GLIBC_MIN`).

## Regenerating generated files

- Icons: `scripts/gen-icons.sh` (SVG written with awk; PNG rendered with `rsvg-convert`,
  `inkscape` or `magick`, whichever exists; the results are committed).
- `THIRD_PARTY_NOTICES.md`: `cargo xtask notices` (`--check` verifies it is current).
