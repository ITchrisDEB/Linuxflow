//! Sway and Hyprland: binding set LIVE by the application with the compositor, through its own
//! command socket. These compositors have no shortcuts portal; the application chooses
//! the key, checks that it is free and releases it on exit.
//!
//! - Sway: i3-ipc protocol (`RUN_COMMAND` for `bindsym`/`unbindsym`, `GET_CONFIG` for conflicts).
//! - Hyprland: `keyword bind` / `keyword unbind` and `j/binds` on the command socket.
//!
//! On press, the compositor runs `linuxflow --toggle`, which passes the command to the running instance
//! (toggle only: no release, hence no push-to-talk this way).
//! Tested against fake sockets; NOT tested on a real Sway or a real Hyprland.

use super::backend::{BackendError, BackendErrorKind, BackendReport, HotkeyBackend, StartRequest};
use super::keymap;
use super::native::gnome_command;
use super::select::BackendKind;
use super::spec::{HotkeySpec, Modifiers};
use crate::inject::hyprland::SocketIpc;
use linuxflow_core::traits::HotkeySink;
use linuxflow_i18n::{tr, tr_fmt};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const IPC_TIMEOUT: Duration = Duration::from_secs(2);

fn err(kind: BackendErrorKind, msg: impl Into<String>) -> BackendError {
    BackendError::new(kind, msg)
}

fn launch_command() -> Result<String, BackendError> {
    let exe =
        crate::autostart::launch_exe().map_err(|e| err(BackendErrorKind::Failed, e.to_string()))?;
    Ok(gnome_command(&exe.to_string_lossy()))
}

// ---------------------------------------------------------------- Sway

const SWAY_MAGIC: &[u8; 6] = b"i3-ipc";
const SWAY_RUN_COMMAND: u32 = 0;
const SWAY_GET_CONFIG: u32 = 9;

/// An i3-ipc exchange: `i3-ipc` + length + type (32-bit integers in native order), then the payload.
fn sway_exchange(socket: &Path, kind: u32, payload: &str) -> Result<String, String> {
    let mut s = UnixStream::connect(socket).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(IPC_TIMEOUT))
        .map_err(|e| e.to_string())?;
    s.set_write_timeout(Some(IPC_TIMEOUT))
        .map_err(|e| e.to_string())?;
    let mut msg = Vec::with_capacity(14 + payload.len());
    msg.extend_from_slice(SWAY_MAGIC);
    msg.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
    msg.extend_from_slice(&kind.to_ne_bytes());
    msg.extend_from_slice(payload.as_bytes());
    s.write_all(&msg).map_err(|e| e.to_string())?;
    let mut header = [0u8; 14];
    s.read_exact(&mut header).map_err(|e| e.to_string())?;
    if &header[..6] != SWAY_MAGIC {
        return Err("unexpected reply from the compositor".into());
    }
    let len = u32::from_ne_bytes([header[6], header[7], header[8], header[9]]) as usize;
    if len > 4 << 20 {
        return Err("reply too large".into());
    }
    let mut body = vec![0u8; len];
    s.read_exact(&mut body).map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// Key in `bindsym` format: `Ctrl+Alt+Shift+Mod4+<keysym>`.
pub fn sway_binding(spec: &HotkeySpec) -> Option<String> {
    let key = keymap::xkb_name(spec.key?)?;
    let mut parts: Vec<&str> = Vec::new();
    for (m, n) in [
        (Modifiers::CTRL, "Ctrl"),
        (Modifiers::ALT, "Alt"),
        (Modifiers::SHIFT, "Shift"),
        (Modifiers::SUPER, "Mod4"),
    ] {
        if spec.modifiers.contains(m) {
            parts.push(n);
        }
    }
    parts.push(key);
    Some(parts.join("+"))
}

/// Canonical form of a binding (sorted modifiers, unified names, lower-case key).
fn sway_canonical(binding: &str) -> String {
    let mut mods: Vec<String> = Vec::new();
    let mut key = String::new();
    for token in binding.split('+').filter(|t| !t.is_empty()) {
        let t = token.to_ascii_lowercase();
        match t.as_str() {
            "ctrl" | "control" => mods.push("ctrl".into()),
            "alt" | "mod1" => mods.push("alt".into()),
            "shift" => mods.push("shift".into()),
            "mod4" | "super" | "logo" => mods.push("super".into()),
            _ => key = t,
        }
    }
    mods.sort();
    mods.dedup();
    format!("{}+{key}", mods.join("+"))
}

/// Replaces the `$name` variables defined by `set $name value` in the configuration.
fn sway_expand(config: &str) -> Vec<String> {
    let mut vars: Vec<(String, String)> = Vec::new();
    for line in config.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("set ") {
            if let Some((name, value)) = rest.trim().split_once(char::is_whitespace) {
                if name.starts_with('$') {
                    vars.push((name.to_string(), value.trim().to_string()));
                }
            }
        }
    }
    config
        .lines()
        .map(|l| {
            let mut line = l.trim().to_string();
            for (n, v) in &vars {
                line = line.replace(n.as_str(), v);
            }
            line
        })
        .collect()
}

/// Command of the Sway configuration that already uses `wanted` (canonical form), if there is one.
fn sway_find_conflict(config: &str, wanted: &str) -> Option<String> {
    for line in sway_expand(config) {
        let mut tokens = line.split_whitespace();
        if !matches!(tokens.next(), Some("bindsym" | "bindcode")) {
            continue;
        }
        let rest: Vec<&str> = tokens.filter(|t| !t.starts_with("--")).collect();
        let Some(binding) = rest.first() else {
            continue;
        };
        if sway_canonical(binding) == wanted {
            return Some(rest[1..].join(" "));
        }
    }
    None
}

/// Attempt WITHOUT binding anything: is the key already in the Sway configuration?
pub fn sway_probe(spec: &HotkeySpec) -> Result<(), String> {
    let Some(socket) = std::env::var_os("SWAYSOCK").filter(|v| !v.is_empty()) else {
        return Ok(());
    };
    let Some(binding) = sway_binding(spec) else {
        return Ok(());
    };
    let Ok(config) = sway_exchange(Path::new(&socket), SWAY_GET_CONFIG, "") else {
        return Ok(());
    };
    let text = serde_json::from_str::<serde_json::Value>(&config)
        .ok()
        .and_then(|v| v["config"].as_str().map(str::to_string))
        .unwrap_or_default();
    match sway_find_conflict(&text, &sway_canonical(&binding)) {
        Some(action) => Err(tr_fmt(
            "{} is already used by your Sway configuration (“{}”). Choose another shortcut.",
            &[&spec.display(), &action],
        )),
        None => Ok(()),
    }
}

/// Attempt WITHOUT binding anything: is the key already bound in Hyprland?
pub fn hyprland_probe(spec: &HotkeySpec) -> Result<(), String> {
    let ipc = SocketIpc::from_env();
    let Some(key) = spec.key.and_then(keymap::xkb_name) else {
        return Ok(());
    };
    let Ok(binds) = ipc.json("binds") else {
        return Ok(());
    };
    let ours = launch_command().unwrap_or_default();
    match hypr_find_conflict(&binds, hypr_modmask(spec.modifiers), key, &ours) {
        Some(action) => Err(tr_fmt(
            "{} is already used by your Hyprland configuration (“{}”). Choose another shortcut.",
            &[&spec.display(), &action],
        )),
        None => Ok(()),
    }
}

pub struct SwayKeysBackend {
    socket: Option<std::path::PathBuf>,
    bound: Option<String>,
}

impl SwayKeysBackend {
    pub fn new() -> Self {
        Self {
            socket: std::env::var_os("SWAYSOCK")
                .filter(|v| !v.is_empty())
                .map(Into::into),
            bound: None,
        }
    }

    /// Explicit socket (tests).
    pub fn with_socket(path: std::path::PathBuf) -> Self {
        Self {
            socket: Some(path),
            bound: None,
        }
    }
}

impl Default for SwayKeysBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// `true` if all the commands of the answer succeeded.
fn sway_reply_ok(reply: &str) -> Result<(), String> {
    let v: serde_json::Value = serde_json::from_str(reply).map_err(|e| e.to_string())?;
    let items = v.as_array().ok_or("unexpected reply")?;
    for it in items {
        if it["success"].as_bool() != Some(true) {
            return Err(it["error"]
                .as_str()
                .unwrap_or("command refused")
                .to_string());
        }
    }
    Ok(())
}

impl HotkeyBackend for SwayKeysBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::SwayKeys
    }

    fn start(
        &mut self,
        req: &StartRequest,
        _sink: Arc<dyn HotkeySink>,
    ) -> Result<BackendReport, BackendError> {
        self.stop();
        let socket = self.socket.clone().ok_or_else(|| {
            err(
                BackendErrorKind::Unavailable,
                tr("Sway's command socket was not found."),
            )
        })?;
        let binding = sway_binding(&req.spec).ok_or_else(|| {
            err(
                BackendErrorKind::Unavailable,
                tr("This shortcut cannot be used with Sway's key bindings."),
            )
        })?;
        let config = sway_exchange(&socket, SWAY_GET_CONFIG, "").map_err(|e| {
            err(
                BackendErrorKind::Unavailable,
                format!("{} ({e})", tr("Sway's command socket is not reachable.")),
            )
        })?;
        let config_text = serde_json::from_str::<serde_json::Value>(&config)
            .ok()
            .and_then(|v| v["config"].as_str().map(str::to_string))
            .unwrap_or_default();
        if let Some(action) = sway_find_conflict(&config_text, &sway_canonical(&binding)) {
            return Err(err(
                BackendErrorKind::Conflict,
                tr_fmt(
                    "{} is already used by your Sway configuration (“{}”). Choose another shortcut.",
                    &[&req.spec.display(), &action],
                ),
            ));
        }
        let command = format!("bindsym {binding} exec {}", launch_command()?);
        let reply = sway_exchange(&socket, SWAY_RUN_COMMAND, &command)
            .map_err(|e| err(BackendErrorKind::Failed, e))?;
        sway_reply_ok(&reply).map_err(|e| err(BackendErrorKind::Failed, format!("Sway: {e}")))?;
        self.bound = Some(binding);
        Ok(BackendReport {
            supports_release: false,
            supports_modifier_only: false,
            trigger_description: req.spec.display(),
            provides_hotkey_events: false,
            provides_raw_keys: false,
            note: Some(tr(
                "Sway starts \"linuxflow --toggle\" when you press the shortcut: press once to start, again to stop.",
            )),
        })
    }

    fn stop(&mut self) {
        if let (Some(binding), Some(socket)) = (self.bound.take(), self.socket.as_ref()) {
            let _ = sway_exchange(socket, SWAY_RUN_COMMAND, &format!("unbindsym {binding}"));
        }
    }
}

impl Drop for SwayKeysBackend {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------------------------------------------------------------- Hyprland

/// Hyprland modifier mask (`modmask` of `j/binds`).
fn hypr_modmask(m: Modifiers) -> u32 {
    let mut mask = 0;
    for (flag, bit) in [
        (Modifiers::SHIFT, 1),
        (Modifiers::CTRL, 4),
        (Modifiers::ALT, 8),
        (Modifiers::SUPER, 64),
    ] {
        if m.contains(flag) {
            mask |= bit;
        }
    }
    mask
}

/// `MODS, key` as expected by `keyword bind` (`CTRL ALT, space`).
pub fn hypr_target(spec: &HotkeySpec) -> Option<String> {
    let key = keymap::xkb_name(spec.key?)?;
    let mut mods: Vec<&str> = Vec::new();
    for (m, n) in [
        (Modifiers::SUPER, "SUPER"),
        (Modifiers::CTRL, "CTRL"),
        (Modifiers::ALT, "ALT"),
        (Modifiers::SHIFT, "SHIFT"),
    ] {
        if spec.modifiers.contains(m) {
            mods.push(n);
        }
    }
    Some(format!("{}, {key}", mods.join(" ")))
}

/// Hyprland binding that already uses (mask, key), other than ours (`our_command`).
fn hypr_find_conflict(binds_json: &str, mask: u32, key: &str, our_command: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(binds_json).ok()?;
    v.as_array()?.iter().find_map(|b| {
        let same = b["modmask"].as_u64() == Some(u64::from(mask))
            && b["key"]
                .as_str()
                .is_some_and(|k| k.eq_ignore_ascii_case(key));
        let arg = b["arg"].as_str().unwrap_or("");
        if same && !arg.contains(our_command) {
            let dispatcher = b["dispatcher"].as_str().unwrap_or("");
            Some(format!("{dispatcher} {arg}").trim().to_string())
        } else {
            None
        }
    })
}

pub struct HyprlandKeysBackend {
    ipc: SocketIpc,
    bound: Option<String>,
}

impl HyprlandKeysBackend {
    pub fn new() -> Self {
        Self {
            ipc: SocketIpc::from_env(),
            bound: None,
        }
    }

    /// Explicit socket (tests).
    pub fn with_socket(path: std::path::PathBuf) -> Self {
        Self {
            ipc: SocketIpc::with_path(path),
            bound: None,
        }
    }
}

impl Default for HyprlandKeysBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl HotkeyBackend for HyprlandKeysBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::HyprlandKeys
    }

    fn start(
        &mut self,
        req: &StartRequest,
        _sink: Arc<dyn HotkeySink>,
    ) -> Result<BackendReport, BackendError> {
        self.stop();
        use crate::inject::hyprland::HyprIpc;
        if !self.ipc.available() {
            return Err(err(
                BackendErrorKind::Unavailable,
                tr("Hyprland's command socket was not found."),
            ));
        }
        let target = hypr_target(&req.spec).ok_or_else(|| {
            err(
                BackendErrorKind::Unavailable,
                tr("This shortcut cannot be used with Hyprland's key bindings."),
            )
        })?;
        let command = launch_command()?;
        let key = keymap::xkb_name(req.spec.key.unwrap_or(0)).unwrap_or("");
        let binds = self.ipc.json("binds").map_err(|e| {
            err(
                BackendErrorKind::Unavailable,
                format!(
                    "{} ({e})",
                    tr("Hyprland's command socket is not reachable.")
                ),
            )
        })?;
        if let Some(action) =
            hypr_find_conflict(&binds, hypr_modmask(req.spec.modifiers), key, &command)
        {
            return Err(err(
                BackendErrorKind::Conflict,
                tr_fmt(
                    "{} is already used by your Hyprland configuration (“{}”). Choose another shortcut.",
                    &[&req.spec.display(), &action],
                ),
            ));
        }
        let reply = self
            .ipc
            .command(&format!("keyword bind {target}, exec, {command}"))
            .map_err(|e| err(BackendErrorKind::Failed, e))?;
        if !reply.trim().eq_ignore_ascii_case("ok") {
            return Err(err(BackendErrorKind::Failed, format!("Hyprland: {reply}")));
        }
        self.bound = Some(target);
        Ok(BackendReport {
            supports_release: false,
            supports_modifier_only: false,
            trigger_description: req.spec.display(),
            provides_hotkey_events: false,
            provides_raw_keys: false,
            note: Some(tr(
                "Hyprland starts \"linuxflow --toggle\" when you press the shortcut: press once to start, again to stop.",
            )),
        })
    }

    fn stop(&mut self) {
        use crate::inject::hyprland::HyprIpc;
        if let Some(target) = self.bound.take() {
            let _ = self.ipc.command(&format!("keyword unbind {target}"));
        }
    }
}

impl Drop for HyprlandKeysBackend {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use linuxflow_core::domain::ShortcutMode;
    use std::os::unix::net::UnixListener;

    struct Null;
    impl HotkeySink for Null {
        fn hotkey(&self, _: linuxflow_core::traits::HotkeyEvent) {}
        fn raw_key(&self, _: linuxflow_core::traits::RawKeyEvent) {}
    }

    fn spec(t: &str) -> HotkeySpec {
        HotkeySpec::parse(t).unwrap()
    }

    fn req(t: &str) -> StartRequest {
        StartRequest {
            spec: spec(t),
            mode: ShortcutMode::Toggle,
        }
    }

    #[test]
    fn sway_bindings_use_sway_modifier_names() {
        assert_eq!(
            sway_binding(&spec("Ctrl+Alt+Space")).as_deref(),
            Some("Ctrl+Alt+space")
        );
        assert_eq!(
            sway_binding(&spec("Super+Shift+d")).as_deref(),
            Some("Shift+Mod4+d")
        );
        assert_eq!(sway_binding(&spec("Ctrl+Super")), None);
    }

    #[test]
    fn a_sway_conflict_is_found_through_variables_and_flags() {
        let config = "set $mod Mod4\\nbindsym $mod+d exec rofi\\nbindsym --release Ctrl+Alt+space exec ptt\\n";
        let config = config.replace("\\n", "\n");
        assert_eq!(
            sway_find_conflict(&config, &sway_canonical("Mod4+d")).as_deref(),
            Some("exec rofi")
        );
        assert_eq!(
            sway_find_conflict(&config, &sway_canonical("Ctrl+Alt+space")).as_deref(),
            Some("exec ptt")
        );
        assert_eq!(
            sway_find_conflict(&config, &sway_canonical("Ctrl+Alt+k")),
            None
        );
        assert_eq!(
            sway_canonical("Control+Mod1+SPACE"),
            sway_canonical("Alt+Ctrl+space")
        );
    }

    #[test]
    fn hyprland_targets_and_masks() {
        assert_eq!(
            hypr_target(&spec("Ctrl+Alt+Space")).as_deref(),
            Some("CTRL ALT, space")
        );
        assert_eq!(
            hypr_target(&spec("Super+Shift+d")).as_deref(),
            Some("SUPER SHIFT, d")
        );
        assert_eq!(hypr_target(&spec("F9")).as_deref(), Some(", F9"));
        assert_eq!(hypr_modmask(spec("Ctrl+Alt+Space").modifiers), 4 | 8);
        assert_eq!(hypr_modmask(spec("Super+Shift+d").modifiers), 64 | 1);
    }

    #[test]
    fn a_hyprland_conflict_ignores_our_own_binding() {
        let binds = r#"[
            {"modmask":12,"key":"space","dispatcher":"exec","arg":"rofi -show"},
            {"modmask":64,"key":"d","dispatcher":"exec","arg":"/opt/linuxflow --toggle"}]"#;
        assert_eq!(
            hypr_find_conflict(binds, 12, "space", "/opt/linuxflow --toggle").as_deref(),
            Some("exec rofi -show")
        );
        assert_eq!(
            hypr_find_conflict(binds, 64, "d", "/opt/linuxflow --toggle"),
            None
        );
        assert_eq!(hypr_find_conflict(binds, 4, "x", "c"), None);
        assert_eq!(hypr_find_conflict("not json", 12, "space", "c"), None);
    }

    /// Fake Sway: answers GET_CONFIG then RUN_COMMAND, records the commands received.
    fn fake_sway(
        dir: &Path,
        config: &'static str,
        exchanges: usize,
    ) -> (std::path::PathBuf, std::thread::JoinHandle<Vec<String>>) {
        let path = dir.join("sway.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let handle = std::thread::spawn(move || {
            let mut commands = Vec::new();
            for _ in 0..exchanges {
                let Ok((mut s, _)) = listener.accept() else {
                    break;
                };
                let mut header = [0u8; 14];
                if s.read_exact(&mut header).is_err() {
                    break;
                }
                assert_eq!(&header[..6], b"i3-ipc");
                let len = u32::from_ne_bytes(header[6..10].try_into().unwrap()) as usize;
                let kind = u32::from_ne_bytes(header[10..14].try_into().unwrap());
                let mut payload = vec![0u8; len];
                s.read_exact(&mut payload).unwrap();
                let reply = if kind == SWAY_GET_CONFIG {
                    serde_json::json!({"config": config}).to_string()
                } else {
                    commands.push(String::from_utf8(payload).unwrap());
                    r#"[{"success":true}]"#.to_string()
                };
                let mut out = b"i3-ipc".to_vec();
                out.extend_from_slice(&(reply.len() as u32).to_ne_bytes());
                out.extend_from_slice(&kind.to_ne_bytes());
                out.extend_from_slice(reply.as_bytes());
                s.write_all(&out).unwrap();
            }
            commands
        });
        (path, handle)
    }

    #[test]
    fn sway_gets_a_binding_through_a_real_socket_and_frees_it() {
        let dir = tempfile::tempdir().unwrap();
        let (path, server) = fake_sway(dir.path(), "bindsym Mod4+d exec rofi\n", 3);
        let mut b = SwayKeysBackend::with_socket(path);
        let report = b
            .start(&req("Ctrl+Alt+Space"), Arc::new(Null))
            .expect("libre");
        assert!(!report.supports_release);
        b.stop();
        let commands = server.join().unwrap();
        assert!(
            commands[0].starts_with("bindsym Ctrl+Alt+space exec "),
            "{commands:?}"
        );
        assert!(commands[0].ends_with("--toggle"), "{commands:?}");
        assert_eq!(commands[1], "unbindsym Ctrl+Alt+space");
    }

    #[test]
    fn sway_refuses_a_key_already_in_the_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let (path, server) = fake_sway(dir.path(), "bindsym Ctrl+Alt+space exec other\n", 1);
        let mut b = SwayKeysBackend::with_socket(path);
        let e = b.start(&req("Ctrl+Alt+Space"), Arc::new(Null)).unwrap_err();
        assert_eq!(e.kind, BackendErrorKind::Conflict);
        assert!(e.message.contains("other"), "{}", e.message);
        drop(b);
        assert!(server.join().unwrap().is_empty(), "nothing must be bound");
    }

    #[test]
    fn hyprland_binds_and_unbinds_through_its_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".socket.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for _ in 0..3 {
                let Ok((mut s, _)) = listener.accept() else {
                    break;
                };
                let mut buf = [0u8; 1024];
                let n = s.read(&mut buf).unwrap();
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let reply = if req.starts_with("j/binds") {
                    "[]"
                } else {
                    "ok"
                };
                s.write_all(reply.as_bytes()).unwrap();
                requests.push(req);
            }
            requests
        });
        let mut b = HyprlandKeysBackend::with_socket(path);
        b.start(&req("Ctrl+Alt+Space"), Arc::new(Null))
            .expect("libre");
        b.stop();
        let requests = server.join().unwrap();
        assert_eq!(requests[0], "j/binds");
        assert!(
            requests[1].starts_with("/keyword bind CTRL ALT, space, exec, "),
            "{requests:?}"
        );
        assert_eq!(requests[2], "/keyword unbind CTRL ALT, space");
    }
}
