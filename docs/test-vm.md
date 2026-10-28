# Testing on a clean GNOME 46 in a virtual machine

LinuxFlow supports GNOME 46 and later, and the AppImage must start on a distribution as old as Ubuntu 24.04. A virtual
machine with a stock Ubuntu 24.04 desktop is the way to check both without touching your own session. This page records
what was needed to do it with libvirt / QEMU (no Docker, no Podman) and what went wrong on the way.

## Create the machine

- ISO: Ubuntu Desktop 24.04 LTS (24.04.5.1 was used), checksum verified against `SHA256SUMS` of `releases.ubuntu.com`.
- 4 vCPU, 8 GB RAM, 40 GB qcow2 disk, UEFI, KVM, NAT network (libvirt's `default` network).
- Create the disk through libvirt (`virsh vol-create-as default <name>.qcow2 40G --format qcow2`): a user usually cannot
  write to `/var/lib/libvirt/images`.
- Define it with `virt-install --print-xml 1 … | virsh define` (the `--cdrom` form prints two documents; the first is the
  install-time one). Put the disk before the CD in the boot order once Ubuntu is installed.
- Video: **plain `vga`** with a VNC display. `virtio-vga` showed "Display output is not active" after installation, and
  SPICE with OpenGL (3D acceleration) gave a black window in virt-manager although the guest was running (`virsh screenshot
  <vm>` showed its desktop). Do not enable 3D acceleration.
- If the NAT network does not start with "Cannot create bridge virbr0: Package not installed", the running kernel's modules
  are gone (a kernel update was installed without a reboot): reboot, then `virsh net-start default`.

## Use it

- Install `openssh-server` in the guest to copy files and run commands (`scp`, `ssh`). Over SSH the session variables are
  missing: set `XDG_RUNTIME_DIR=/run/user/<uid>`, `WAYLAND_DISPLAY=wayland-0` and
  `DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/<uid>/bus` before launching a graphical program.
- Look at the guest without a viewer: `virsh screenshot <vm> file.png`. Send keys with `virsh send-key <vm> KEY_LEFTCTRL
  KEY_LEFTALT KEY_SPACE`.
- GNOME blanks and locks the screen after a few minutes, which shows as a black screen: `gsettings set
  org.gnome.desktop.screensaver lock-enabled false` and `gsettings set org.gnome.desktop.session idle-delay 0` in the guest.
- A USB camera can be handed to the running machine with `virsh attach-device <vm> <hostdev.xml> --live` (it disappears from
  the host while attached).
- To replace the AppImage in the guest, quit the running one first (`<AppImage> --quit`), otherwise the file is busy.

## What to check

1. The AppImage starts when run directly, with no extra environment variable.
2. Troubleshooting page: no warning on a plain GNOME 46 (see `docs/gnome-support.md`); the dock shows the LinuxFlow icon
   (the application registers its launcher entry and icon at start-up, see below).
3. The shortcut is registered (`gsettings get org.gnome.settings-daemon.plugins.media-keys custom-keybindings`) and a press of
   Ctrl+Alt+Space starts and stops the listening.
4. Dictation and paste into a text editor. The first paste shows GNOME's "Remote Desktop" dialog: enable both switches and
   keep "Remember this choice" checked.

## Known noise

Ubuntu 24.04's portal service can crash right after a paste (Apport shows "Sorry, Ubuntu 24.04 has experienced an internal
error"); see `docs/gnome-support.md`. It is the desktop's own service, and LinuxFlow reconnects.

## AppImage launcher entry

A program that is only run as an AppImage is not installed, and Wayland takes the dock icon from a `.desktop` file for the
window's application id. At start-up LinuxFlow therefore writes, when `$APPIMAGE` is set, the launcher entry
(`~/.local/share/applications/io.github.ITchrisDEB.LinuxFlow.desktop`, starting the `.AppImage` file, with the same actions
as the installed entry) and the icons (`~/.local/share/icons/hicolor/*/apps/`). It follows the file if it is moved and does
nothing outside an AppImage (`crates/linuxflow-platform/src/appimage.rs`). A launcher entry in the user's directory takes
priority over the system one, so when the application is installed system-wide the entry is not written and an earlier
one made by an AppImage is removed (otherwise the menu would keep showing the AppImage). To undo it, delete those files.
