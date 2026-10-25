//! Real implementation of `Env`. None of these probes opens an input device, asks
//! for a permission or writes anything.

use super::env::{Env, WaylandGlobal};
use super::groups::{group_names_for, parse_group_file, parse_status_gids};
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

const PROBE_TIMEOUT: Duration = Duration::from_secs(4);

pub struct SystemEnv;

/// Runs `f` in a dedicated thread with a maximum delay (a faulty compositor or Vulkan loader
/// must never block the start-up). The orphan thread ends by itself.
fn with_timeout<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    with_timeout_for(PROBE_TIMEOUT, f)
}

fn with_timeout_for<T: Send + 'static>(
    timeout: Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("lf-probe".into())
        .spawn(move || {
            let _ = tx.send(f());
        })
        .ok()?;
    rx.recv_timeout(timeout).ok()
}

/// GLOBAL budget of all the D-Bus probes, counted from the first one: a blocked bus can
/// no longer delay the start-up by (number of probes × delay), only by this budget.
const DBUS_BUDGET: Duration = Duration::from_secs(6);

static DBUS_FIRST_USE: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
/// True as soon as a connection to the session bus has failed or timed out: the following probes
/// answer "unknown" immediately instead of retrying.
static DBUS_DOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn dbus_remaining() -> Duration {
    let started = *DBUS_FIRST_USE.get_or_init(std::time::Instant::now);
    DBUS_BUDGET.saturating_sub(started.elapsed())
}

/// Runs a D-Bus future in an ephemeral runtime (dedicated thread: safe even when called from tokio).
fn dbus<T: Send + 'static, F>(
    make: impl FnOnce(ashpd::zbus::Connection) -> F + Send + 'static,
) -> Option<T>
where
    F: std::future::Future<Output = Option<T>>,
{
    use std::sync::atomic::Ordering;
    let remaining = dbus_remaining();
    if DBUS_DOWN.load(Ordering::SeqCst) || remaining.is_zero() {
        return None;
    }
    let step = remaining.min(Duration::from_secs(3));
    with_timeout_for(remaining.min(PROBE_TIMEOUT), move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;
        rt.block_on(async {
            let conn = match tokio::time::timeout(step, ashpd::zbus::Connection::session()).await {
                Ok(Ok(conn)) => conn,
                _ => {
                    DBUS_DOWN.store(true, Ordering::SeqCst);
                    return None;
                }
            };
            tokio::time::timeout(step, make(conn)).await.ok()?
        })
    })
    .flatten()
}

impl Env for SystemEnv {
    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok().filter(|v| !v.is_empty())
    }

    fn path_exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn path_writable(&self, path: &Path) -> bool {
        use std::os::unix::ffi::OsStrExt;
        let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: `c` is a valid NUL-terminated C string; `access` does not keep the pointer.
        unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
    }

    fn wayland_globals(&self) -> Option<Vec<WaylandGlobal>> {
        with_timeout(wayland_registry).flatten()
    }

    fn dbus_name_has_owner(&self, name: &str) -> Option<bool> {
        let name = name.to_string();
        dbus(move |conn| async move {
            let proxy = ashpd::zbus::fdo::DBusProxy::new(&conn).await.ok()?;
            let bus_name = ashpd::zbus::names::BusName::try_from(name).ok()?;
            proxy.name_has_owner(bus_name).await.ok()
        })
    }

    fn portal_interface_version(&self, interface: &str) -> Option<u32> {
        let iface = format!("org.freedesktop.portal.{interface}");
        dbus(move |conn| async move {
            let proxy = ashpd::zbus::Proxy::new(
                &conn,
                "org.freedesktop.portal.Desktop",
                "/org/freedesktop/portal/desktop",
                iface,
            )
            .await
            .ok()?;
            proxy.get_property::<u32>("version").await.ok()
        })
    }

    fn user_groups(&self) -> Vec<String> {
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        let group_file = std::fs::read_to_string("/etc/group").unwrap_or_default();
        group_names_for(&parse_status_gids(&status), &parse_group_file(&group_file))
    }

    fn vulkan_devices(&self) -> Result<Vec<String>, String> {
        // Already bounded (child process with a maximum delay): no extra thread.
        vulkan_devices()
    }
}

fn wayland_registry() -> Option<Vec<WaylandGlobal>> {
    use wayland_client::globals::{registry_queue_init, GlobalListContents};
    use wayland_client::protocol::wl_registry;
    use wayland_client::{Connection, Dispatch, QueueHandle};

    struct Probe;
    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Probe {
        fn event(
            _: &mut Self,
            _: &wl_registry::WlRegistry,
            _: wl_registry::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    let conn = Connection::connect_to_env().ok()?;
    let (list, _queue) = registry_queue_init::<Probe>(&conn).ok()?;
    Some(list.contents().with_list(|globals| {
        globals
            .iter()
            .map(|g| WaylandGlobal {
                interface: g.interface.clone(),
                version: g.version,
            })
            .collect()
    }))
}

/// Lists the REAL Vulkan GPUs in an isolated child process.
///
/// No native Vulkan call takes place in the main process: a faulty driver
/// (NVIDIA, Mesa…) may crash, and that must never kill the application. We therefore run
/// `linuxflow-engine --hardware-probe` (or, failing that, the current executable with the same argument),
/// with a maximum delay, then interrupt it if it does not answer.
fn vulkan_devices() -> Result<Vec<String>, String> {
    let exe = probe_executable().ok_or_else(|| "no probe executable found".to_string())?;
    let output = run_with_deadline(&exe, &["--hardware-probe"], PROBE_DEADLINE)?;
    parse_probe_output(&output)
}

const PROBE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// Executable that can answer `--hardware-probe`: the light engine installed next to the main
/// binary, otherwise the current executable (which also handles this argument).
fn probe_executable() -> Option<std::path::PathBuf> {
    let current = std::env::current_exe().ok()?;
    if let Some(sibling) = current.parent().map(|d| d.join("linuxflow-engine")) {
        if sibling.is_file() {
            return Some(sibling);
        }
    }
    Some(current)
}

/// Runs `exe args…`, reads its standard output and interrupts it (SIGKILL) after `deadline`.
fn run_with_deadline(
    exe: &std::path::Path,
    args: &[&str],
    deadline: std::time::Duration,
) -> Result<String, String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::Instant;

    use std::os::unix::process::CommandExt;
    let mut child = Command::new(exe)
        .args(args)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut stdout = child.stdout.take().ok_or("no stdout")?;
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() >= deadline => break None,
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e.to_string());
            }
        }
    };
    let Some(status) = status else {
        // Blocked driver: the whole group is killed; the reader ends by itself when the pipe is closed.
        // SAFETY: `kill` on the group created by us; no memory involved.
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
        let _ = child.wait();
        drop(reader);
        return Err("Vulkan probe timed out".into());
    };
    let text = reader.join().unwrap_or_default();
    if status.success() {
        Ok(text)
    } else {
        Err(format!("Vulkan probe failed ({status})"))
    }
}

/// Names of the real GPUs from the JSON output of `--hardware-probe` (software drivers are discarded).
fn parse_probe_output(text: &str) -> Result<Vec<String>, String> {
    let line = text
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .ok_or_else(|| "empty probe output".to_string())?;
    let value: serde_json::Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
    if let Some(err) = value.get("vulkan_error").and_then(|e| e.as_str()) {
        return Err(err.to_string());
    }
    let gpus = value
        .get("gpus")
        .and_then(|g| g.as_array())
        .ok_or_else(|| "no gpus field".to_string())?;
    Ok(gpus
        .iter()
        .filter(|g| {
            !matches!(
                g.get("kind").and_then(|k| k.as_str()),
                Some("Software" | "Cpu" | "Other")
            )
        })
        .filter_map(|g| g.get("name").and_then(|n| n.as_str()).map(str::to_owned))
        .collect())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod probe_tests {
    use super::*;

    #[test]
    fn probe_output_keeps_real_gpus_only() {
        let json = r#"{"gpus":[{"name":"NVIDIA GeForce RTX","kind":"Discrete"},{"name":"llvmpipe","kind":"Software"},{"name":"AMD iGPU","kind":"Integrated"}],"vulkan_error":null}"#;
        assert_eq!(
            parse_probe_output(json).unwrap(),
            vec!["NVIDIA GeForce RTX".to_string(), "AMD iGPU".to_string()]
        );
    }

    #[test]
    fn probe_output_reports_a_vulkan_error() {
        let json = r#"{"gpus":[],"vulkan_error":"libvulkan not found"}"#;
        assert_eq!(parse_probe_output(json).unwrap_err(), "libvulkan not found");
        assert!(parse_probe_output("garbage").is_err());
    }

    #[test]
    fn dbus_probes_fail_fast_once_the_bus_is_known_to_be_down() {
        use std::sync::atomic::Ordering;
        // Regression: blocked bus = up to 3 s per probe × 5 probes. Now: immediate answer.
        DBUS_DOWN.store(true, Ordering::SeqCst);
        let started = std::time::Instant::now();
        for _ in 0..5 {
            let out: Option<u32> = dbus(|_conn| async { Some(1u32) });
            assert_eq!(out, None);
        }
        assert!(started.elapsed() < Duration::from_millis(200));
        DBUS_DOWN.store(false, Ordering::SeqCst);
    }

    #[test]
    fn a_hung_probe_is_killed_at_the_deadline() {
        let started = std::time::Instant::now();
        let err = run_with_deadline(
            std::path::Path::new("/bin/sleep"),
            &["30"],
            std::time::Duration::from_millis(300),
        )
        .unwrap_err();
        assert!(err.contains("timed out"), "{err}");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn a_crashing_probe_is_an_error_not_a_crash() {
        // `false` exits with an error code: the application must simply receive an Err.
        let err = run_with_deadline(
            std::path::Path::new("/bin/false"),
            &[],
            std::time::Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(err.contains("failed"), "{err}");
    }
}
