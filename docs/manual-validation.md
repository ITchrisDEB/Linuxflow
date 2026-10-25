# Manual validation (what no automated test can prove)

The automated tests cover the pure logic, downloads against a local server, the installation scripts and offscreen window rendering. They can NOT prove that LinuxFlow works on a real desktop. Run this list on every target environment and record each result (OK / KO / conditional + remark) in the final table.

Prerequisites: the installed archive (`./install.sh`, default prefix `~/.local`), a microphone, and an installed model (first-launch wizard). Logs: `$XDG_STATE_HOME/linuxflow/logs` (default `~/.local/state/linuxflow/logs`), level adjustable with `LINUXFLOW_LOG=debug`. No log may contain the dictated text, the clipboard or a token.

## 0. Environments to cover
KDE Plasma 6 (Wayland) · GNOME 48+ (Wayland) · GNOME < 48 · Sway · Hyprland · an X11 desktop (Xfce, Cinnamon or MATE), with AND without a compositor. Note the distribution and the versions (`lsb_release -a`; the GTK and libadwaita versions are shown in the Troubleshooting page).

## 1. Start-up and degradation
1. `linuxflow --version` prints the version without opening a window.
2. Normal launch: the hub opens; the Troubleshooting page lists every component (session, portals, layer-shell, Vulkan, PipeWire, tray…) with its state and a hint when one is missing.
3. Launch in a degraded environment and check that it still starts, with a clear message: no portal (empty `DBUS_SESSION_BUS_ADDRESS=`), no GPU (`LINUXFLOW_DEVICE=cpu`), no network (`LINUXFLOW_OFFLINE=1`), no microphone (no input device).
4. Second launch (`linuxflow` again): opens the existing hub and does not create a second instance. `linuxflow --page settings` opens the requested page.
5. Close the hub window: the application keeps running (tray icon / pill still active). `linuxflow --quit` quits cleanly (no `linuxflow-engine` process left: `pgrep -a linuxflow`).

## 2. First launch and models
1. Wizard: the detected hardware is correct; the recommended model, its size and the disk space are shown BEFORE any download. Without a usable Vulkan GPU, the large model is NEVER offered as recommended.
2. Real download of the small model: the progress bar advances, and the progress also shows in the tray tooltip and the pill.
3. Cut the network (airplane mode or `ip link set`) during the download, then restore it: the download RESUMES where it stopped (no restart from zero).
4. Corrupted checksum: change one byte of the downloaded file, then "Verify integrity": the error is reported.
5. "Use a file I already have" with a model that is already present: accepted (size and checksum verified).
6. First launch offline (`LINUXFLOW_OFFLINE=1`) without a model: clear message, the application stays usable, no network request (`ss -tp | grep linuxflow` is empty).

## 3. End-to-end dictation (per environment)
For each case, dictate a sentence of 5 to 10 seconds.
1. Shortcut (default Ctrl+Alt+Space, or the one you chose): the pill appears at the bottom of the screen, listens, then shows "Done"; the text is pasted into: a text editor, a browser field, a terminal (Ctrl+Shift+V expected), a chat application.
2. The pill does NOT STEAL focus: the window that receives the text stays the one where the cursor blinks (`xdotool getwindowfocus` under X11).
3. The clipboard is restored (copy "before" prior to dictating, check that it comes back) and non-text content (an image) is not silently destroyed.
4. "Hold to talk" mode: short press (< 300 ms) → hands-free; hold → stops on release; another key added within 700 ms → cancel. Note whether the key release is detected (GNOME without a recent portal: "not available" is expected, with a message shown).
5. Paste refused (RemoteDesktop portal refused, or an administrator window): the text stays in the clipboard, a "Paste blocked" message appears, no stray keystroke.
6. Keyboard layouts: AZERTY AND Dvorak/QWERTY: pasting sends "V", not another letter.
7. Released modifiers: dictate while keeping Ctrl pressed for a moment at the end; the text must not arrive as "Ctrl+Alt+V".
8. Long dictation (300 s, the maximum duration): clean stop; dictation shorter than 250 ms: "Nothing heard".
9. "Try it here" box on the hub home page: the text arrives in the test area without touching other windows.

## 4. Recording pill
1. KDE/Sway/Hyprland (layer-shell): above all windows, including full screen; click-through (clicking on the pill acts on the window below).
2. GNOME: the pill shows (XWayland child process); note whether it goes above full-screen windows; it does not take focus.
3. X11 without a compositor: readable opaque pill (no unexpected black background).
4. HiDPI screen (scale 1.5 and 2): sharp, well-centred pill; several screens: reasonable placement.

## 5. Tray icon
1. KDE Plasma: the icon is visible and is the LinuxFlow logo (dark rounded square, white bars); right click = menu (status header, start/stop/cancel, Open LinuxFlow, Language, Microphone, "Paste automatically" and "Launch at login" check boxes, About, Quit); left click = opens the hub; middle click = toggles dictation; the bars turn GREEN while listening and the icon does not blink; the colour stays the same on dark and light themes.
2. GNOME with the "AppIndicator and KStatusNotifierItem Support" extension: same checks. Enable the extension AFTER LinuxFlow has started: the icon must appear without a restart. Restart the shell (Alt+F2, `r`, under X11): the icon reappears.
3. GNOME without the extension: the application stays usable; the Troubleshooting page and a single toast explain how to get the icon; right-clicking LinuxFlow in the dock/Activities offers "Start or stop dictation", "Cancel", "Settings", "Quit" (actions of the .desktop file).
4. Sway/Hyprland with waybar (tray module): same checks as KDE.
5. Change the language or the microphone from the menu: the hub reflects the change.

## 5b. Weak microphone
1. Settings > "Weak microphone": Automatic at first; dictate quietly with a microphone set low: no word lost at the start of the sentence. Also check that a normal dictation is not changed; switch to Off and observe that the first word may be lost.
2. Choose +20 dB with a loud voice: the text may degrade (saturation); this is expected, go back to Automatic.
3. Choose Off, restart the application: the choice is kept.

## 6. Engine, GPU and hardware
1. Archive built with the GPU engine (default): dictation runs on the GPU (Settings > Local engine names the GPU). Already verified on a recent NVIDIA discrete GPU; to redo on AMD (Mesa RADV) and Intel. Measure: 10 s of audio in under 2 s on a common GPU; note model, GPU and time.
2. Force a device: `LINUXFLOW_DEVICE=gpu:1` (second GPU), `LINUXFLOW_DEVICE=cpu`. `LINUXFLOW_DEVICE=cuda` must give an explicit error (no silent fallback).
3. Machine WITHOUT a Vulkan loader (minimal VM): only `linuxflow-engine-vulkan` is linked to `libvulkan.so.1` (`readelf -d` on each binary); the application and the CPU engine start, the compatibility report flags the missing Vulkan and the light model is used. (Verified by simulation: with an unusable `libvulkan.so.1`, the CPU engine and hardware detection answer normally.)
4. CPU without AVX2: the `linuxflow-engine-v2` engine is chosen and works (6 to 7 times slower expected); never an "illegal instruction" crash (`dmesg | grep -i "invalid opcode"`).
5. Kill the engine (`pkill -9 linuxflow-engine`) while listening: the application does not crash, shows "Engine stopped (code …)", restarts the engine (at most twice) and works again.
6. Memory peak: `ps -o rss,cmd -C linuxflow-engine` during a dictation (≈ 1 GB for small, ≈ 2.2 GB for large-v3 q5_0).

## 7. Privacy and security
1. `ls -l ~/.config/linuxflow ~/.local/share/linuxflow ~/.local/state/linuxflow`: files are 0600, directories 0700.
2. Network: apart from the model download, NO outgoing connection (`ss -tnp | grep linuxflow`, or `strace -f -e trace=connect`). No local port listening (`ss -ltnp | grep linuxflow` is empty).
3. History: turn history off in the settings: no more entries written; "Clear all" empties the file.
4. Nothing abnormal in the logs (no dictated text, no portal token).
5. evdev/uinput: disabled by default; enabling shows an explicit warning about reading the keyboard.

## 8. Installation, update, uninstallation
1. `./install.sh --dry-run` creates nothing; real installation; the application appears in the application menu (icon + name).
2. Reinstall over the top: clean; `./uninstall.sh`: no residual file (`find ~/.local -name '*linuxflow*'`), user data kept; `--purge` asks for confirmation.
3. "Launch at login" enabled in the settings: restart the session, the application starts in the background (`--background`) without showing the hub.
4. System older than the baseline (GTK < 4.12 or libadwaita < 1.4): `install.sh` refuses with a message pointing to BUILD.md.

## 8b. Appearance (light / dark)
1. Settings → Appearance = System (default): switch the desktop between its light and dark styles while LinuxFlow is open: the hub, the wizard and the dialogs follow immediately, with no restart. Check KDE Plasma, GNOME and at least one other desktop; note the ones whose appearance setting LinuxFlow does not follow (it then stays light).
2. Appearance = Light or Dark: the choice overrides the desktop and is kept after a restart.
3. In both themes, read every page (Home, History, Dictionary, Snippets, Models, Settings, Troubleshooting) and the wizard: no unreadable text, no white block in the dark theme, no dark block in the light theme; the shortcut highlight on the Home page is readable.
4. The recording pill and the tray icon stay readable on a light and on a dark panel/background.

## 9. Languages
1. `LANGUAGE=fr linuxflow`: the whole interface, the tray, the pill and the error messages are in French; no residual English text; no truncation. `LANGUAGE=en`: everything in English.
2. Settings → Interface language (Automatic / English / Français): the change applies immediately in the hub, the tray and the pill. The wizard also offers the language on its first step.
3. French numbers with a narrow no-break space ("1 280"), dates and relative times in the right language.

## 10. Results table to fill in
| Environment | Start-up | Dictation + paste | Hands-free / PTT | Pill | Tray | GPU | Autostart | Notes |
|---|---|---|---|---|---|---|---|---|
| KDE Plasma 6 Wayland | | | | | | | | |
| GNOME 48+ Wayland | | | | | | | | |
| GNOME < 48 | | | | | | | | |
| Sway | | | | | | | | |
| Hyprland | | | | | | | | |
| X11 (with compositor) | | | | | | | | |
| X11 (without compositor) | | | | | | | | |
