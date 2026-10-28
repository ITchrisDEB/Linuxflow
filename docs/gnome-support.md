# GNOME support (46 and later)

LinuxFlow supports GNOME 46 and later (GTK 4.14, libadwaita 1.5 and later), on Wayland and on X11.
GNOME 46 is the version shipped with Ubuntu 24.04 LTS, which is the oldest base LinuxFlow is built for
(glibc 2.39, see `packaging/MIN_VERSIONS`). The same code path is used on every later GNOME version: the features
below are detected at run time, not selected by version number.

## What differs between GNOME versions

| Feature | GNOME 46 (checked in a VM) | GNOME 47 and later (detected at run time, not checked here) | What LinuxFlow does |
|---|---|---|---|
| GlobalShortcuts portal | absent | may be present (expected from GNOME 48) | On GNOME (Wayland) LinuxFlow does not depend on it: it registers its own shortcut as a GNOME custom keyboard shortcut (`org.gnome.settings-daemon.plugins.media-keys`) that runs `linuxflow --toggle`. The portal is only a second choice. |
| Push-to-talk (hold) | toggle only | toggle only on the custom shortcut | GNOME does not send the key release to a custom shortcut, so the hold mode is not available on this path. Hands-free (press to start, press to stop) works. |
| RemoteDesktop portal | present | expected present | Used to send the paste key (the user is asked for consent once). |
| Clipboard portal | present | expected present | Used to place the text and restore the clipboard. |
| `wlr-layer-shell` | not offered | not expected (GNOME does not implement it) | The recording pill runs as a small child process under XWayland, as a regular window. |
| `wlr-data-control` | not offered | not expected (GNOME does not implement it) | Not needed: the clipboard portal is used. |
| Tray icon | needs the "AppIndicator and KStatusNotifierItem Support" extension (enabled by default on Ubuntu) | same | Ubuntu enables it by default; elsewhere install and enable it. Without it LinuxFlow stays fully usable from its window. |
| Vulkan GPU | depends on the machine | depends on the machine | Without a usable GPU (for example in a virtual machine) the lighter model runs on the CPU. |

## The compatibility report

The Troubleshooting page lists each component. The labels mean:

- **Available**: detected and used.
- **Not available** with a warning icon, and an entry under "How to fix": a real limitation on this machine
  (no way to register the shortcut, no paste or clipboard path, no notification-area host).
- **Not needed**, **Not offered**, **Not applicable**, **Not used**: the component does not apply here or is
  replaced by another mechanism. The reason is shown under the title. This is not a fault.
- **CPU mode**: no usable GPU; speech recognition runs on the CPU.
- **Optional**: a feature that is off by default (`/dev/uinput`, membership of the `input` group, which lets a
  program read every key you type). The installer never changes your groups.

On GNOME 46 you should therefore see no warning except possibly the notification area when the AppIndicator
extension is not enabled.

The decision is made in one place, `Capabilities` (`crates/linuxflow-core/src/domain.rs`): `has_native_shortcuts`,
`can_register_shortcut` and `can_copy_text`. The detection (`crates/linuxflow-platform/src/capabilities`) and the report
(`crates/linuxflow-gtk/src/hub/pages/compat.rs`) both use them, so a component can not be a warning in one and fine in the
other. The unit tests cover GNOME without any optional component, X11, and an unsupported Wayland desktop.

## Known issue: the portal service can crash on GNOME 46

On Ubuntu 24.04 (`xdg-desktop-portal` 1.18.4, `xdg-desktop-portal-gnome` 46.2) the desktop's portal service sometimes aborts
a moment after a paste: `xdg-desktop-portal-gnome` dies on a GLib assertion
(`g_variant_type_info_check: assertion failed: (0 <= index && index < 24)`), then `xdg-desktop-portal` crashes with SIGSEGV.
Ubuntu shows "Sorry, Ubuntu 24.04 has experienced an internal error" (Apport). This was seen in the virtual machine right
after a successful paste, and the same symptom is reported by another project on GNOME 46
(<https://github.com/vladkens/nimbo/pull/34>). The crash happens inside the desktop's own services; the cause on
LinuxFlow's side has not been found, and it did not happen on every paste.

What LinuxFlow does about it:
- Sending the keys already recovered from a lost portal session (new session, no new dialog thanks to the restore token).
- Reading or setting the clipboard now does the same: a failed call drops the session and is repeated once on a new one.
  Before, the clipboard stayed unusable until LinuxFlow was restarted.

What it can not do: stop Ubuntu from showing the crash report. Choose "Do not send" in the dialog.

## Detecting the desktop

The desktop is read from `XDG_CURRENT_DESKTOP`. When that variable is missing (program started from an autostart entry, a
service or a remote shell that did not inherit the session environment) LinuxFlow falls back to `DESKTOP_SESSION` /
`GDMSESSION`, then to the owner of the shell's well-known name on the session bus (`org.gnome.Shell`, `org.kde.KWin`). Without
this, the Troubleshooting page showed an unknown desktop and the GNOME shortcut was not registered.

## Testing

A clean Ubuntu 24.04 (GNOME 46) virtual machine and what to check in it: [test-vm.md](test-vm.md).

## Build requirement

The AppImage bundles GTK 4 and libadwaita from Ubuntu 24.04 (see `xtask/src/appimage.rs`) and the LinuxFlow binaries
require glibc 2.39 at most. They therefore start on Ubuntu 24.04 (GNOME 46) and on any more recent distribution.

## Verified

Ubuntu 24.04.5 desktop in a virtual machine (GNOME Shell 46.0, Wayland, GTK 4.14.5, libadwaita 1.5.0,
`xdg-desktop-portal` 1.18.4, `xdg-desktop-portal-gnome` 46.2, PipeWire, no GPU), LinuxFlow 0.3.0 started from the AppImage:

- The Troubleshooting page shows no warning: the GlobalShortcuts portal, layer shell and data control are "Not needed" /
  "Not offered", the GPU is "CPU mode", `uinput` and the `input` group are "Optional". Before the correction all of them
  were "Not available" warnings.
- The shortcut is registered through the GNOME custom shortcuts (Ctrl+Alt+Space, command `linuxflow --toggle`), also when the
  program is started without `XDG_CURRENT_DESKTOP` (desktop recognised from the shell's bus name).
- Pressing Ctrl+Alt+Space on the virtual keyboard starts the dictation (status "Listening…", log line `dictation:
  listening`), and pressing it again stops it.

## Not verified yet

- GNOME 47, 48 and later: the logic is the same and covered by unit tests with simulated environments, but it has not
  been run on those versions.
- Pasting the transcribed text into another application, the recording pill, the tray icon appearance and autostart on
  GNOME 46 (the VM has no microphone input to dictate real speech).
- Other desktops were not run for this change. Their shortcut paths are unchanged and covered by the existing automated
  tests (KDE: `KGlobalAccel`; Sway and Hyprland: compositor socket; X11: key grab; others: GlobalShortcuts portal, then the
  optional evdev capture). On a Wayland desktop with none of these, bind a desktop shortcut to `linuxflow --toggle`.
