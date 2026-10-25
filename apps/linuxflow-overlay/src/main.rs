//! LinuxFlow recording pill for Wayland compositors that offer layer-shell (KDE Plasma,
//! Sway, Hyprland, COSMIC…), in a separate process.
//!
//! Why a separate program: `libgtk4-layer-shell` is not packaged by every distribution (Ubuntu
//! 24.04 LTS, RHEL 10…) and cannot be loaded on demand once GTK has started. Isolated here,
//! its absence only costs the floating pill (LinuxFlow then falls back to XWayland), but
//! no longer prevents the application from starting.
//!
//! Launched by `linuxflow`, which sends it the pill states on its standard input.

use std::process::ExitCode;

fn main() -> ExitCode {
    // Application probe: if this code runs, the dynamic loader found all the
    // libraries, `libgtk4-layer-shell` included.
    if std::env::args().nth(1).as_deref() == Some(linuxflow_gtk::overlay::PROBE_ARG) {
        return ExitCode::SUCCESS;
    }
    linuxflow_gtk::overlay::worker_main()
}
