//! The HTTP layer on a real socket: HTTP/1.0 answers with the headers the
//! page relies on, http.server's error pages, a JSON write, and the stream
//! (its headers and first event).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use scw_rsi::{FetchError, Reply, SystemClock, Transport};
use scw_store::paths::{Mode, Roots};
use scw_ui::{Hooks, Ui, http};

struct NoNet;

impl Transport for NoNet {
    fn get(&self, url: &str, _: &str, _: f64) -> Result<Reply, FetchError> {
        panic!("no network in this test: {url}")
    }
}

fn ask(port: u16, raw: &str) -> (String, Vec<(String, String)>, Vec<u8>) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.write_all(raw.as_bytes()).unwrap();
    let mut r = BufReader::new(s);
    let mut status = String::new();
    r.read_line(&mut status).unwrap();
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        r.read_line(&mut h).unwrap();
        let h = h.trim_end().to_string();
        if h.is_empty() {
            break;
        }
        let (k, v) = h.split_once(": ").unwrap();
        headers.push((k.to_string(), v.to_string()));
    }
    let mut body = Vec::new();
    if headers
        .iter()
        .any(|(k, v)| k == "Content-Type" && v == "text/event-stream")
    {
        // one event, then leave
        loop {
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            body.extend_from_slice(line.as_bytes());
            if line == "\n" && body.len() > 1 {
                break;
            }
        }
    } else {
        r.read_to_end(&mut body).unwrap();
    }
    (status.trim_end().to_string(), headers, body)
}

fn header<'a>(h: &'a [(String, String)], k: &str) -> Option<&'a str> {
    h.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
}

#[test]
fn http_as_python_http_server_speaks_it() {
    let base = std::env::temp_dir().join(format!("scw-ui-http-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("data")).unwrap();
    let hooks = Hooks {
        spawn: Box::new(|job| job()),
        reveal: Box::new(|_| Ok(())),
        speak: Box::new(|_| Ok(())),
        input: Box::new(|| panic!("no input here")),
        env: Arc::new(|_| None),
        today: Box::new(|| chrono::NaiveDate::from_ymd_opt(2026, 11, 22).unwrap()),
        utc: true,
    };
    let roots = Roots {
        mode: Mode::Chosen,
        program: base.join("program"),
        data: base.join("data"),
    };
    let ui = Arc::new(Ui::new(
        roots,
        Arc::new(SystemClock),
        Arc::new(NoNet),
        hooks,
    ));
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    {
        let ui = ui.clone();
        std::thread::spawn(move || http::serve(ui, listener));
    }

    let (st, h, body) = ask(port, "GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
    assert_eq!(st, "HTTP/1.0 200 OK");
    assert_eq!(header(&h, "Content-Type"), Some("text/html; charset=utf-8"));
    assert_eq!(header(&h, "Cache-Control"), Some("no-store"));
    let page = std::fs::read(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ui/index.html"))
        .unwrap();
    assert_eq!(body, page, "the page is the Python's, byte for byte");
    assert_eq!(
        header(&h, "Content-Length"),
        Some(page.len().to_string().as_str())
    );

    let (st, h, body) = ask(
        port,
        "GET /nothing HTTP/1.1\r\nHost: localhost:8731\r\n\r\n",
    );
    assert_eq!(st, "HTTP/1.0 404 Not Found");
    assert_eq!(header(&h, "Connection"), Some("close"));
    assert!(String::from_utf8_lossy(&body).contains("<p>Error code: 404</p>"));

    let (st, _, _) = ask(port, "GET / HTTP/1.1\r\nHost: evil.example\r\n\r\n");
    assert_eq!(st, "HTTP/1.0 403 not a loopback host");

    let json = r#"{"handle": "exmplone"}"#;
    let (st, h, body) = ask(
        port,
        &format!(
            "POST /api/flag HTTP/1.1\r\nHost: [::1]:8731\r\nContent-Type: application/json; charset=utf-8\r\n\
             Content-Length: {}\r\n\r\n{json}",
            json.len()
        ),
    );
    assert_eq!(st, "HTTP/1.0 200 OK");
    assert_eq!(header(&h, "Content-Type"), Some("application/json"));
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["handle"], "EXMPLONE");

    let (st, _, _) = ask(
        port,
        "POST /api/flag HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: text/plain\r\nContent-Length: 2\r\n\r\n{}",
    );
    assert_eq!(st, "HTTP/1.0 415 expected application/json");

    let (st, h, body) = ask(port, "GET /api/stream HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
    assert_eq!(st, "HTTP/1.0 200 OK");
    assert_eq!(header(&h, "Content-Type"), Some("text/event-stream"));
    assert_eq!(header(&h, "Connection"), Some("keep-alive"));
    assert_eq!(header(&h, "X-Accel-Buffering"), Some("no"));
    let text = String::from_utf8(body).unwrap();
    let event = text.strip_prefix("data: ").unwrap().trim_end();
    let state: serde_json::Value = serde_json::from_str(event).unwrap();
    assert_eq!(
        state["stats"]["pending"], 1,
        "the flag above, on the stream"
    );

    // The page's "text only, no screenshots" button: a bundle, said whole.
    std::fs::create_dir_all(base.join("data/debug_bursts")).unwrap();
    std::fs::write(
        base.join("data/debug_bursts/session_20261122_100000.log"),
        "a log\n",
    )
    .unwrap();
    std::fs::write(base.join("data/debug_bursts/burst_100000_0.png"), "x").unwrap();
    let json = r#"{"frames": false}"#;
    let (st, _, body) = ask(
        port,
        &format!(
            "POST /api/support-bundle HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\n\r\n{json}",
            json.len()
        ),
    );
    assert_eq!(st, "HTTP/1.0 200 OK");
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(
        (v["logs"].as_u64(), v["images"].as_u64()),
        (Some(1), Some(0))
    );
    assert_eq!(v["with_frames"], false);
    assert!(
        v["manifest"]
            .as_str()
            .unwrap()
            .contains("screenshots      NONE - built with --no-frames")
    );
    let zip = PathBuf::from(v["file"].as_str().unwrap());
    assert!(zip.starts_with(base.join("data").join("debug_bursts")) && zip.is_file());
    assert!(v["mb"].is_number());

    ui.shutting_down.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&base);
}
