#![allow(clippy::unwrap_used, clippy::expect_used)]
//! T-DL: download with a local HTTP server (no real network access).

mod support;

use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use linuxflow_core::domain::ModelState;
use linuxflow_models::{
    Catalog, Config, Integrity, ModelError, ModelManager, ProgressCallback, ProgressPhase,
    ProgressUpdate,
};
use support::*;

const LEN: usize = 600_000;

fn cfg() -> Config {
    Config {
        system_proxy: false,
        retry_base: Duration::from_millis(10),
        retry_max: Duration::from_millis(40),
        stall_timeout: Duration::from_secs(2),
        connect_timeout: Duration::from_secs(2),
        progress_interval: Duration::ZERO,
        disk_margin_bytes: 0,
        ..Config::default()
    }
}

fn catalog(urls: &[String], len: usize, sha: Option<String>) -> Catalog {
    let urls: Vec<String> = urls.iter().map(|u| format!("\"{u}\"")).collect();
    let sha = sha
        .map(|s| format!("\"sha256\":\"{s}\","))
        .unwrap_or_default();
    let json = format!(
        r#"{{"endpoint":"https://hf.test","models":[{{"id":"m","kind":"stt","file":"m.bin","download_bytes":{len},{sha}"urls":[{}]}}]}}"#,
        urls.join(",")
    );
    let c = Catalog::from_json(&json).unwrap();
    assert_eq!(c.entries().len(), 1, "{:?}", c.warnings());
    c
}

type Events = Arc<Mutex<Vec<ProgressUpdate>>>;

fn manager(dir: &Path, cat: Catalog, cfg: Config) -> (ModelManager, Events) {
    let ev: Events = Arc::new(Mutex::new(Vec::new()));
    let e2 = ev.clone();
    let cb: ProgressCallback = Arc::new(move |u| e2.lock().unwrap().push(u));
    (ModelManager::new(dir, cat, cfg).with_progress(cb), ev)
}

fn leftovers(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n != "m.bin" && n != "m.bin.lock")
        .collect();
    v.sort();
    v
}

fn assert_monotone(ev: &Events) {
    let g = ev.lock().unwrap();
    let mut last = 0;
    for u in g.iter().filter(|u| u.phase != ProgressPhase::Verifying) {
        assert!(
            u.done >= last,
            "progression non monotone : {} puis {}",
            last,
            u.done
        );
        assert!(u.done <= u.total);
        last = u.done;
    }
}

async fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < Duration::from_secs(10), "timeout: {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn state(m: &ModelManager) -> ModelState {
    m.statuses().remove(0).state
}

#[tokio::test]
async fn full_download_atomic_and_verified() {
    let body = Arc::new(make_body(LEN));
    let b = body.clone();
    let srv = Server::start(move |r, s, _| {
        serve_file(
            r,
            s,
            &b,
            &FileOpts {
                etag: Some("\"v1\"".into()),
                range: true,
                ..Default::default()
            },
        )
    });
    let dir = tempfile::tempdir().unwrap();
    let cat = catalog(&[srv.url("/m.bin")], LEN, Some(sha256_hex(&body)));
    let (m, ev) = manager(dir.path(), cat, cfg());
    assert_eq!(state(&m), ModelState::NotInstalled);
    let path = m.download("m").await.unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), *body);
    assert_eq!(
        leftovers(dir.path()),
        Vec::<String>::new(),
        "ni .part ni .etag"
    );
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o644
    );
    assert_eq!(state(&m), ModelState::Installed);
    assert_eq!(m.model_path("m"), Some(path));
    assert_monotone(&ev);
    let last = ev.lock().unwrap().last().cloned().unwrap();
    assert_eq!((last.phase, last.percent()), (ProgressPhase::Finished, 100));
    assert!(ev
        .lock()
        .unwrap()
        .iter()
        .any(|u| u.phase == ProgressPhase::Verifying));
    // Second call: already installed, no further request.
    let hits = srv.hits();
    m.download("m").await.unwrap();
    assert_eq!(srv.hits(), hits);
}

#[tokio::test]
async fn request_identity_is_minimal() {
    let body = make_body(1000);
    let b = body.clone();
    let srv = Server::start(move |r, s, _| serve_file(r, s, &b, &FileOpts::default()));
    let dir = tempfile::tempdir().unwrap();
    let (m, _) = manager(
        dir.path(),
        catalog(&[srv.url("/m.bin")], 1000, Some(sha256_hex(&body))),
        cfg(),
    );
    m.download("m").await.unwrap();
    let r = srv.requests.lock().unwrap()[0].clone();
    assert!(r.headers["user-agent"].starts_with("linuxflow-models/"));
    for h in ["cookie", "authorization", "referer", "x-user"] {
        assert!(!r.headers.contains_key(h), "unexpected header {h}");
    }
}

#[tokio::test]
async fn cut_midway_then_resume_with_range() {
    let body = Arc::new(make_body(LEN));
    let b = body.clone();
    let srv = Server::start(move |r, s, n| {
        let o = FileOpts {
            etag: Some("\"v1\"".into()),
            range: true,
            cut_at: (n == 0).then_some(LEN * 4 / 10),
            ..Default::default()
        };
        serve_file(r, s, &b, &o)
    });
    let dir = tempfile::tempdir().unwrap();
    let (m, ev) = manager(
        dir.path(),
        catalog(&[srv.url("/m.bin")], LEN, Some(sha256_hex(&body))),
        cfg(),
    );
    let path = m.download("m").await.unwrap();
    assert_eq!(std::fs::read(path).unwrap(), *body);
    let reqs = srv.requests.lock().unwrap().clone();
    assert_eq!(reqs.len(), 2);
    let range = reqs[1].headers.get("range").expect("Range on resume");
    let n: usize = range
        .trim_start_matches("bytes=")
        .trim_end_matches('-')
        .parse()
        .unwrap();
    assert!(n > 0 && n <= LEN * 4 / 10, "resume at {n}");
    assert_eq!(
        reqs[1].headers.get("if-range").map(String::as_str),
        Some("\"v1\"")
    );
    assert_monotone(&ev);
}

#[tokio::test]
async fn resumes_existing_part_after_restart() {
    let body = Arc::new(make_body(LEN));
    let b = body.clone();
    let srv = Server::start(move |r, s, _| {
        serve_file(
            r,
            s,
            &b,
            &FileOpts {
                etag: Some("\"v1\"".into()),
                range: true,
                ..Default::default()
            },
        )
    });
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("m.bin.part"), &body[..250_000]).unwrap();
    std::fs::write(dir.path().join("m.bin.part.etag"), "\"v1\"").unwrap();
    let (m, _) = manager(
        dir.path(),
        catalog(&[srv.url("/m.bin")], LEN, Some(sha256_hex(&body))),
        cfg(),
    );
    assert_eq!(
        state(&m),
        ModelState::Paused {
            done: 250_000,
            total: LEN as u64
        }
    );
    m.download("m").await.unwrap();
    assert_eq!(
        srv.requests.lock().unwrap()[0].headers["range"],
        "bytes=250000-"
    );
    assert_eq!(std::fs::read(dir.path().join("m.bin")).unwrap(), *body);
}

#[tokio::test]
async fn server_without_range_restarts_cleanly() {
    let body = Arc::new(make_body(LEN));
    let b = body.clone();
    let srv = Server::start(move |r, s, n| {
        let o = FileOpts {
            range: false,
            cut_at: (n == 0).then_some(LEN / 3),
            ..Default::default()
        };
        serve_file(r, s, &b, &o)
    });
    let dir = tempfile::tempdir().unwrap();
    let (m, ev) = manager(
        dir.path(),
        catalog(&[srv.url("/m.bin")], LEN, Some(sha256_hex(&body))),
        cfg(),
    );
    m.download("m").await.unwrap();
    assert_eq!(std::fs::read(dir.path().join("m.bin")).unwrap(), *body);
    let reqs = srv.requests.lock().unwrap().clone();
    assert!(
        reqs[1].headers.contains_key("range"),
        "the resume was attempted"
    );
    drop(ev);
}

#[tokio::test]
async fn http_416_restarts_from_scratch() {
    let body = Arc::new(make_body(LEN));
    let b = body.clone();
    let srv = Server::start(move |r, s, _| {
        if r.headers.contains_key("range") {
            send_simple(
                s,
                "416 Range Not Satisfiable",
                &[("Content-Range", &format!("bytes */{LEN}"))],
                b"",
            );
        } else {
            serve_file(r, s, &b, &FileOpts::default());
        }
    });
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("m.bin.part"), &body[..1000]).unwrap();
    let (m, _) = manager(
        dir.path(),
        catalog(&[srv.url("/m.bin")], LEN, Some(sha256_hex(&body))),
        cfg(),
    );
    m.download("m").await.unwrap();
    assert_eq!(std::fs::read(dir.path().join("m.bin")).unwrap(), *body);
    assert_eq!(srv.hits(), 2);
}

#[tokio::test]
async fn changed_etag_restarts_whether_server_answers_200_or_206() {
    for ignore_if_range in [false, true] {
        let body = Arc::new(make_body(LEN));
        let b = body.clone();
        let srv = Server::start(move |r, s, _| {
            let o = FileOpts {
                etag: Some("\"new\"".into()),
                range: true,
                ignore_if_range,
                ..Default::default()
            };
            serve_file(r, s, &b, &o)
        });
        let dir = tempfile::tempdir().unwrap();
        // The start of the `.part` comes from another version of the file.
        std::fs::write(dir.path().join("m.bin.part"), vec![7u8; 100_000]).unwrap();
        std::fs::write(dir.path().join("m.bin.part.etag"), "\"old\"").unwrap();
        let (m, _) = manager(
            dir.path(),
            catalog(&[srv.url("/m.bin")], LEN, Some(sha256_hex(&body))),
            cfg(),
        );
        m.download("m").await.unwrap();
        assert_eq!(
            std::fs::read(dir.path().join("m.bin")).unwrap(),
            *body,
            "ignore_if_range={ignore_if_range}"
        );
    }
}

#[tokio::test]
async fn wrong_sha256_is_rejected_and_part_removed() {
    let body = make_body(50_000);
    let b = body.clone();
    let srv = Server::start(move |r, s, _| serve_file(r, s, &b, &FileOpts::default()));
    let dir = tempfile::tempdir().unwrap();
    let wrong = "0".repeat(64);
    let (m, _) = manager(
        dir.path(),
        catalog(&[srv.url("/m.bin")], 50_000, Some(wrong)),
        cfg(),
    );
    let err = m.download("m").await.unwrap_err();
    assert!(matches!(err, ModelError::ChecksumMismatch { .. }), "{err}");
    assert!(!dir.path().join("m.bin").exists());
    assert_eq!(leftovers(dir.path()), Vec::<String>::new());
    assert!(matches!(state(&m), ModelState::Error(_)));
    assert!(!err.user_message().is_empty());
}

#[tokio::test]
async fn redirect_is_followed() {
    let body = make_body(30_000);
    let b = body.clone();
    let srv = Server::start(move |r, s, _| {
        if r.path == "/start" {
            send_simple(s, "302 Found", &[("Location", "/cdn/final.bin")], b"");
        } else {
            serve_file(r, s, &b, &FileOpts::default());
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let (m, _) = manager(
        dir.path(),
        catalog(&[srv.url("/start")], 30_000, Some(sha256_hex(&body))),
        cfg(),
    );
    m.download("m").await.unwrap();
    assert_eq!(srv.hits(), 2);
    assert_eq!(srv.requests.lock().unwrap()[1].path, "/cdn/final.bin");
}

#[tokio::test]
async fn failing_mirror_falls_back_to_next() {
    let body = make_body(30_000);
    let b = body.clone();
    let srv = Server::start(move |r, s, _| match r.path.as_str() {
        "/gone" => send_simple(s, "404 Not Found", &[], b"nope"),
        "/busy" => send_simple(s, "503 Service Unavailable", &[], b""),
        _ => serve_file(r, s, &b, &FileOpts::default()),
    });
    let dir = tempfile::tempdir().unwrap();
    let urls = [srv.url("/gone"), srv.url("/busy"), srv.url("/good")];
    let c = cfg();
    let attempts = c.max_attempts as usize;
    let (m, _) = manager(
        dir.path(),
        catalog(&urls, 30_000, Some(sha256_hex(&body))),
        c,
    );
    m.download("m").await.unwrap();
    let paths: Vec<String> = srv
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|r| r.path.clone())
        .collect();
    assert_eq!(
        paths.iter().filter(|p| *p == "/gone").count(),
        1,
        "404: no new attempt"
    );
    assert_eq!(
        paths.iter().filter(|p| *p == "/busy").count(),
        attempts,
        "503: attempts exhausted"
    );
    assert_eq!(paths.last().unwrap(), "/good");
}

#[tokio::test]
async fn all_mirrors_failing_reports_last_error() {
    let srv = Server::start(|_, s, _| send_simple(s, "403 Forbidden", &[], b""));
    let dir = tempfile::tempdir().unwrap();
    let (m, _) = manager(
        dir.path(),
        catalog(&[srv.url("/a"), srv.url("/b")], 100, None),
        cfg(),
    );
    let err = m.download("m").await.unwrap_err();
    assert!(matches!(err, ModelError::Http(403)), "{err}");
    assert!(err.user_message().contains("403"));
    assert_eq!(srv.hits(), 2);
}

#[tokio::test]
async fn backoff_waits_longer_each_time() {
    let srv = Server::start(|_, s, _| send_simple(s, "503 Service Unavailable", &[], b""));
    let dir = tempfile::tempdir().unwrap();
    let mut c = cfg();
    c.retry_base = Duration::from_millis(60);
    c.retry_max = Duration::from_secs(5);
    c.max_attempts = 4;
    let (m, _) = manager(dir.path(), catalog(&[srv.url("/a")], 100, None), c);
    let t = Instant::now();
    let _ = m.download("m").await.unwrap_err();
    // 60 + 120 + 240 ms of accumulated wait at least.
    assert!(
        t.elapsed() >= Duration::from_millis(400),
        "{:?}",
        t.elapsed()
    );
    assert_eq!(srv.hits(), 4);
}

#[tokio::test]
async fn truncated_response_without_range_support_is_retried() {
    let body = Arc::new(make_body(LEN));
    let b = body.clone();
    let srv = Server::start(move |r, s, n| {
        let o = FileOpts {
            cut_at: (n < 2).then_some(1000 * (n + 1)),
            ..Default::default()
        };
        serve_file(r, s, &b, &o)
    });
    let dir = tempfile::tempdir().unwrap();
    let (m, _) = manager(
        dir.path(),
        catalog(&[srv.url("/m.bin")], LEN, Some(sha256_hex(&body))),
        cfg(),
    );
    m.download("m").await.unwrap();
    assert_eq!(srv.hits(), 3);
}

#[tokio::test]
async fn stalled_server_times_out_then_succeeds() {
    let body = Arc::new(make_body(20_000));
    let b = body.clone();
    let srv = Server::start(move |r, s, n| {
        serve_file(
            r,
            s,
            &b,
            &FileOpts {
                stall: n == 0,
                ..Default::default()
            },
        )
    });
    let dir = tempfile::tempdir().unwrap();
    let mut c = cfg();
    c.stall_timeout = Duration::from_millis(300);
    let (m, _) = manager(
        dir.path(),
        catalog(&[srv.url("/m.bin")], 20_000, Some(sha256_hex(&body))),
        c,
    );
    let t = Instant::now();
    m.download("m").await.unwrap();
    assert!(
        t.elapsed() < Duration::from_millis(2500),
        "{:?}",
        t.elapsed()
    );
    assert_eq!(srv.hits(), 2);
}

#[tokio::test]
async fn silent_server_header_timeout() {
    // The server accepts but sends no header.
    let srv = Server::start(|_, _s, _| std::thread::sleep(Duration::from_secs(2)));
    let dir = tempfile::tempdir().unwrap();
    let mut c = cfg();
    c.stall_timeout = Duration::from_millis(200);
    c.max_attempts = 2;
    let (m, _) = manager(dir.path(), catalog(&[srv.url("/m.bin")], 1000, None), c);
    let err = m.download("m").await.unwrap_err();
    assert!(matches!(err, ModelError::Network(_)), "{err}");
}

fn slow_server(body: Arc<Vec<u8>>) -> Server {
    Server::start(move |r, s, _| {
        let o = FileOpts {
            etag: Some("\"v1\"".into()),
            range: true,
            chunk_delay: Some(Duration::from_millis(3)),
            ..Default::default()
        };
        serve_file(r, s, &body, &o)
    })
}

fn done_of(m: &ModelManager) -> u64 {
    match state(m) {
        ModelState::Downloading { done, .. } | ModelState::Paused { done, .. } => done,
        _ => 0,
    }
}

#[tokio::test]
async fn pause_resume_cancel() {
    let body = Arc::new(make_body(1_000_000));
    let srv = slow_server(body.clone());
    let dir = tempfile::tempdir().unwrap();
    let (m, ev) = manager(
        dir.path(),
        catalog(&[srv.url("/m.bin")], 1_000_000, Some(sha256_hex(&body))),
        cfg(),
    );

    let task = tokio::spawn({
        let m = m.clone();
        async move { m.download("m").await }
    });
    wait_for("start", || done_of(&m) > 0).await;
    assert!(matches!(state(&m), ModelState::Downloading { .. }));
    assert!(m.pause("m"));
    wait_for("pause", || matches!(state(&m), ModelState::Paused { .. })).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let frozen = done_of(&m);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(done_of(&m), frozen, "no byte during the pause");
    assert!(dir.path().join("m.bin.part").exists(), "`.part` kept");
    assert!(m.resume("m"));
    let path = task.await.unwrap().unwrap();
    assert_eq!(std::fs::read(path).unwrap(), *body);
    assert_monotone(&ev);
    assert!(ev
        .lock()
        .unwrap()
        .iter()
        .any(|u| u.phase == ProgressPhase::Paused));
    assert!(srv
        .requests
        .lock()
        .unwrap()
        .iter()
        .skip(1)
        .all(|r| r.headers.contains_key("range")));

    // Cancellation during the download.
    m.delete("m").unwrap();
    let task = tokio::spawn({
        let m = m.clone();
        async move { m.download("m").await }
    });
    wait_for("start 2", || done_of(&m) > 0).await;
    assert!(m.cancel("m"));
    let err = task.await.unwrap().unwrap_err();
    assert!(matches!(err, ModelError::Cancelled));
    assert_eq!(leftovers(dir.path()), Vec::<String>::new());
    assert_eq!(state(&m), ModelState::NotInstalled);

    // Cancellation during a pause.
    let task = tokio::spawn({
        let m = m.clone();
        async move { m.download("m").await }
    });
    wait_for("start 3", || done_of(&m) > 0).await;
    m.pause("m");
    wait_for("pause 3", || matches!(state(&m), ModelState::Paused { .. })).await;
    m.cancel("m");
    assert!(matches!(task.await.unwrap(), Err(ModelError::Cancelled)));
    assert_eq!(leftovers(dir.path()), Vec::<String>::new());
}

#[tokio::test]
async fn cancel_removes_orphan_part_without_job() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("m.bin.part"), b"abc").unwrap();
    let (m, _) = manager(
        dir.path(),
        catalog(&["http://127.0.0.1:9/x".into()], 100, None),
        cfg(),
    );
    assert!(m.cancel("m"));
    assert_eq!(leftovers(dir.path()), Vec::<String>::new());
}

#[tokio::test]
async fn two_managers_only_one_downloads() {
    let body = Arc::new(make_body(1_000_000));
    let srv = slow_server(body.clone());
    let dir = tempfile::tempdir().unwrap();
    let mk = || {
        manager(
            dir.path(),
            catalog(&[srv.url("/m.bin")], 1_000_000, Some(sha256_hex(&body))),
            cfg(),
        )
        .0
    };
    let (a, b) = (mk(), mk());
    let task = tokio::spawn({
        let a = a.clone();
        async move { a.download("m").await }
    });
    wait_for("start", || done_of(&a) > 0).await;
    let err = b.download("m").await.unwrap_err();
    assert!(matches!(err, ModelError::AlreadyDownloading), "{err}");
    let err = a.download("m").await.unwrap_err();
    assert!(
        matches!(err, ModelError::AlreadyDownloading),
        "same instance: {err}"
    );
    task.await.unwrap().unwrap();
    assert_eq!(srv.hits(), 1, "a single download");
    // The second manager sees the installed file.
    assert_eq!(state(&b), ModelState::Installed);
}

#[tokio::test]
async fn insufficient_disk_space_is_reported_before_any_request() {
    let srv = Server::start(|_, s, _| send_simple(s, "200 OK", &[], b""));
    let dir = tempfile::tempdir().unwrap();
    let (m, _) = manager(
        dir.path(),
        catalog(&[srv.url("/m.bin")], 1_000_000, None),
        cfg(),
    );
    let m = m.with_free_space_fn(Arc::new(|_| Ok(999_999)));
    let err = m.download("m").await.unwrap_err();
    assert!(
        matches!(
            err,
            ModelError::InsufficientSpace {
                needed: 1_000_000,
                available: 999_999
            }
        ),
        "{err}"
    );
    assert_eq!(srv.hits(), 0);
    assert!(err.user_message().contains("1.0 MB"));
    assert!(matches!(state(&m), ModelState::Error(_)));
}

#[tokio::test]
async fn disk_space_accounts_for_existing_part_and_margin() {
    let body = Arc::new(make_body(10_000));
    let b = body.clone();
    let srv = Server::start(move |r, s, _| {
        serve_file(
            r,
            s,
            &b,
            &FileOpts {
                range: true,
                ..Default::default()
            },
        )
    });
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("m.bin.part"), &body[..6000]).unwrap();
    let mut c = cfg();
    c.disk_margin_bytes = 500;
    let (m, _) = manager(
        dir.path(),
        catalog(&[srv.url("/m.bin")], 10_000, Some(sha256_hex(&body))),
        c,
    );
    // 4000 left + 500 margin = 4500 required.
    let m = m.with_free_space_fn(Arc::new(|_| Ok(4499)));
    assert!(matches!(
        m.download("m").await,
        Err(ModelError::InsufficientSpace { needed: 4500, .. })
    ));
}

#[tokio::test]
async fn offline_mode_makes_zero_connections() {
    let srv = Server::start(|_, s, _| send_simple(s, "200 OK", &[], b""));
    let dir = tempfile::tempdir().unwrap();
    let c = Config::from_lookup(|k| (k == "HF_HUB_OFFLINE").then(|| "1".to_string()));
    let mut c2 = cfg();
    c2.offline = c.offline;
    let (m, _) = manager(dir.path(), catalog(&[srv.url("/m.bin")], 1000, None), c2);
    assert!(m.is_offline());
    let err = m.download("m").await.unwrap_err();
    assert!(matches!(err, ModelError::Offline));
    assert!(err.user_message().contains("Offline"));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(srv.connections.load(Ordering::SeqCst), 0);
    // A model that is already present stays usable offline.
    std::fs::write(dir.path().join("m.bin"), vec![0u8; 1000]).unwrap();
    assert!(m.download("m").await.is_ok());
    assert_eq!(srv.connections.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn endpoint_override_replaces_official_origin_only() {
    let body = make_body(5000);
    let b = body.clone();
    let srv = Server::start(move |r, s, _| serve_file(r, s, &b, &FileOpts::default()));
    let dir = tempfile::tempdir().unwrap();
    let cat = catalog(
        &["https://hf.test/org/repo/resolve/main/m.bin".into()],
        5000,
        Some(sha256_hex(&body)),
    );
    let mut c = cfg();
    c.endpoint = Some(srv.base());
    let (m, _) = manager(dir.path(), cat, c);
    m.download("m").await.unwrap();
    assert_eq!(
        srv.requests.lock().unwrap()[0].path,
        "/org/repo/resolve/main/m.bin"
    );
}

#[tokio::test]
async fn unverified_entry_installs_as_unverified() {
    let body = make_body(5000);
    let b = body.clone();
    let srv = Server::start(move |r, s, _| serve_file(r, s, &b, &FileOpts::default()));
    let dir = tempfile::tempdir().unwrap();
    let (m, _) = manager(dir.path(), catalog(&[srv.url("/m.bin")], 5000, None), cfg());
    m.download("m").await.unwrap();
    assert_eq!(state(&m), ModelState::Unverified);
    assert!(matches!(
        m.verify("m").await.unwrap(),
        Integrity::NoReferenceSum { .. }
    ));
}

#[tokio::test]
async fn verify_detects_tampering_and_delete_cleans() {
    let body = make_body(40_000);
    let b = body.clone();
    let srv = Server::start(move |r, s, _| serve_file(r, s, &b, &FileOpts::default()));
    let dir = tempfile::tempdir().unwrap();
    let (m, _) = manager(
        dir.path(),
        catalog(&[srv.url("/m.bin")], 40_000, Some(sha256_hex(&body))),
        cfg(),
    );
    m.download("m").await.unwrap();
    assert_eq!(m.verify("m").await.unwrap(), Integrity::Valid);
    let p = dir.path().join("m.bin");
    let mut data = std::fs::read(&p).unwrap();
    data[123] ^= 0xff;
    std::fs::write(&p, &data).unwrap();
    assert!(matches!(
        m.verify("m").await.unwrap(),
        Integrity::Corrupted { .. }
    ));
    assert!(matches!(
        state(&m),
        ModelState::Installed | ModelState::Error(_)
    ));
    m.set_in_use(Some("m"));
    assert!(matches!(m.delete("m"), Err(ModelError::Busy)));
    m.set_in_use(None);
    m.delete("m").unwrap();
    assert_eq!(state(&m), ModelState::NotInstalled);
    assert!(matches!(m.delete("m"), Err(ModelError::NotInstalled)));
    assert!(matches!(m.verify("m").await, Err(ModelError::NotInstalled)));
    assert!(matches!(
        m.download("nope").await,
        Err(ModelError::UnknownModel(_))
    ));
}

#[tokio::test]
async fn use_local_file_checks_then_links() {
    let body = make_body(8000);
    let src_dir = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let models = dir.path().join("models");
    let (m, _) = manager(
        &models,
        catalog(
            &["http://127.0.0.1:9/x".into()],
            8000,
            Some(sha256_hex(&body)),
        ),
        cfg(),
    );

    let good = src_dir.path().join("any-name.bin");
    std::fs::write(&good, &body).unwrap();
    let wrong_size = src_dir.path().join("short.bin");
    std::fs::write(&wrong_size, &body[..10]).unwrap();
    let mut other = body.clone();
    other[0] ^= 1;
    let wrong_sum = src_dir.path().join("other.bin");
    std::fs::write(&wrong_sum, &other).unwrap();

    assert!(matches!(
        m.use_local_file("m", &src_dir.path().join("absent")).await,
        Err(ModelError::InvalidFile(_))
    ));
    assert!(matches!(
        m.use_local_file("m", src_dir.path()).await,
        Err(ModelError::InvalidFile(_))
    ));
    assert!(matches!(
        m.use_local_file("m", &wrong_size).await,
        Err(ModelError::InvalidFile(_))
    ));
    assert!(matches!(
        m.use_local_file("m", &wrong_sum).await,
        Err(ModelError::ChecksumMismatch { .. })
    ));
    assert_eq!(state(&m), ModelState::NotInstalled);

    let dest = m.use_local_file("m", &good).await.unwrap();
    assert_eq!(dest, models.join("m.bin"));
    assert_eq!(std::fs::read(&dest).unwrap(), body);
    assert_eq!(state(&m), ModelState::Installed);
    // Idempotent, and deleting the model does not touch the original.
    m.use_local_file("m", &dest).await.unwrap();
    m.delete("m").unwrap();
    assert!(good.exists());
}

#[tokio::test]
async fn statuses_expose_catalog_and_flags() {
    let dir = tempfile::tempdir().unwrap();
    let m = ModelManager::new(dir.path().join("none"), Catalog::embedded(), cfg());
    m.set_recommended(Some("small"));
    let st = m.statuses();
    assert_eq!(st.len(), 4);
    let small = st.iter().find(|s| s.id == "small").unwrap();
    assert!(small.recommended && !small.in_use);
    assert_eq!(small.download_bytes, 487_601_967);
    assert!(st.iter().all(|s| s.state == ModelState::NotInstalled));
    assert_eq!(m.disk_usage(), 0);
}

#[tokio::test]
async fn use_local_file_follows_symlinks_relative_and_absolute() {
    let body = make_body(8000);
    let src_dir = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let models = dir.path().join("models");
    let real = src_dir.path().join("real.bin");
    std::fs::write(&real, &body).unwrap();
    let rel = src_dir.path().join("rel-link.bin");
    std::os::unix::fs::symlink("real.bin", &rel).unwrap();
    let abs = src_dir.path().join("abs-link.bin");
    std::os::unix::fs::symlink(&real, &abs).unwrap();
    for link in [&rel, &abs] {
        let (m, _) = manager(
            &models,
            catalog(
                &["http://127.0.0.1:9/x".into()],
                8000,
                Some(sha256_hex(&body)),
            ),
            cfg(),
        );
        let dest = m.use_local_file("m", link).await.unwrap();
        let md = std::fs::symlink_metadata(&dest).unwrap();
        assert!(
            md.is_file() && !md.file_type().is_symlink(),
            "the destination must be a regular file"
        );
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert_eq!(state(&m), ModelState::Installed);
        assert!(m.model_path("m").is_some());
        m.delete("m").unwrap();
        assert!(real.exists());
    }
}

#[tokio::test]
async fn corrupted_file_is_not_presented_as_installed() {
    let body = make_body(40_000);
    let b = body.clone();
    let srv = Server::start(move |r, s, _| serve_file(r, s, &b, &FileOpts::default()));
    let dir = tempfile::tempdir().unwrap();
    let (m, _) = manager(
        dir.path(),
        catalog(&[srv.url("/m.bin")], 40_000, Some(sha256_hex(&body))),
        cfg(),
    );
    m.download("m").await.unwrap();
    let p = dir.path().join("m.bin");
    let mut data = std::fs::read(&p).unwrap();
    data[5] ^= 0xff;
    std::fs::write(&p, &data).unwrap();
    assert!(matches!(
        m.verify("m").await.unwrap(),
        Integrity::Corrupted { .. }
    ));
    assert!(matches!(state(&m), ModelState::Error(_)), "{:?}", state(&m));
    assert_eq!(
        m.model_path("m"),
        None,
        "the engine must not load this file"
    );
    // File replaced: the remembered error no longer applies.
    tokio::time::sleep(Duration::from_millis(30)).await;
    std::fs::write(&p, &body).unwrap();
    assert_eq!(state(&m), ModelState::Installed);
    assert!(m.model_path("m").is_some());
    assert_eq!(m.verify("m").await.unwrap(), Integrity::Valid);
}

#[tokio::test]
async fn cancel_does_not_touch_part_held_by_another_process() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("m.bin.part"), b"abc").unwrap();
    let other = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(dir.path().join("m.bin.lock"))
        .unwrap();
    other.try_lock().unwrap();
    let (m, _) = manager(
        dir.path(),
        catalog(&["http://127.0.0.1:9/x".into()], 100, None),
        cfg(),
    );
    assert!(!m.cancel("m"));
    assert!(dir.path().join("m.bin.part").exists());
    assert!(matches!(m.delete("m"), Err(ModelError::Busy)));
    drop(other);
    assert!(m.cancel("m"));
    assert!(!dir.path().join("m.bin.part").exists());
}

#[tokio::test]
async fn lock_file_is_cleaned_after_download_failure_and_delete() {
    let body = make_body(5000);
    let b = body.clone();
    let srv = Server::start(move |r, s, _| serve_file(r, s, &b, &FileOpts::default()));
    let dir = tempfile::tempdir().unwrap();
    let lock = dir.path().join("m.bin.lock");
    let (m, _) = manager(
        dir.path(),
        catalog(&[srv.url("/m.bin")], 5000, Some(sha256_hex(&body))),
        cfg(),
    );
    m.download("m").await.unwrap();
    assert!(!lock.exists(), "lock removed after download");
    m.delete("m").unwrap();
    assert!(!lock.exists(), "lock removed after deletion");
    let (m2, _) = manager(
        dir.path(),
        catalog(&[srv.url("/m.bin")], 5000, Some("0".repeat(64))),
        cfg(),
    );
    assert!(m2.download("m").await.is_err());
    assert!(!lock.exists(), "lock removed after failure");
}
