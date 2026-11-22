//! The HTTPS transport against a small server on 127.0.0.1 (plain HTTP - the
//! same code path past TLS): what it sends, and how it reads each kind of
//! answer, the way urllib reads them. No internet needed.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use scw_rsi::{FetchError, Https, Transport};
use serde_json::json;

fn respond(mut s: TcpStream, seen: Arc<Mutex<Vec<String>>>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        match s.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let req = String::from_utf8_lossy(&buf).to_string();
    let path = req.split_whitespace().nth(1).unwrap_or("").to_string();
    seen.lock().unwrap().push(req);
    let (status, extra, body): (&str, &str, &[u8]) = match path.as_str() {
        "/ok" => (
            "200 OK",
            "RateLimit: \"default\";r=5;t=9\r\nContent-Type: application/json\r\n",
            b"{\"a\": 1, \"b\": [true, null]}",
        ),
        "/missing" => ("404 Not Found", "X-Lookup-State: not_found\r\n", b"nope"),
        "/busy" => ("429 Too Many Requests", "Retry-After: 37\r\n", b"{}"),
        "/junk" => ("200 OK", "", b"not json"),
        "/latin" => ("200 OK", "", b"{\"name\": \"caf\xe9\"}"),
        "/moved" => ("302 Found", "Location: /ok\r\n", b""),
        "/slow" => {
            thread::sleep(Duration::from_secs(2));
            ("200 OK", "", b"{}")
        }
        _ => ("500 Internal Server Error", "", b""),
    };
    let head = format!(
        "HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = s.write_all(head.as_bytes());
    let _ = s.write_all(body);
}

#[test]
fn reads_answers_as_urllib_does() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    thread::spawn(move || {
        for s in listener.incoming().flatten() {
            let seen = s2.clone();
            thread::spawn(move || respond(s, seen));
        }
    });
    let net = Https::new();
    let get = |p: &str, t: f64| net.get(&format!("{base}{p}"), "scw-test/1 (+contact)", t);

    let ok = get("/ok", 5.0).unwrap();
    assert_eq!(ok.body, json!({"a": 1, "b": [true, null]}));
    assert_eq!(ok.headers.get("ratelimit"), Some("\"default\";r=5;t=9"));
    {
        let req = seen.lock().unwrap()[0].to_lowercase();
        assert!(
            req.contains("user-agent: scw-test/1 (+contact)\r\n"),
            "{req}"
        );
        assert!(req.contains("accept: application/json\r\n"), "{req}");
    }

    match get("/missing", 5.0) {
        Err(FetchError::Http { code: 404, headers }) => {
            assert_eq!(headers.get("X-Lookup-State"), Some("not_found"))
        }
        other => panic!("404: {other:?}"),
    }
    match get("/busy", 5.0) {
        Err(FetchError::Http { code: 429, headers }) => {
            assert_eq!(headers.get("Retry-After"), Some("37"))
        }
        other => panic!("429: {other:?}"),
    }
    assert!(matches!(
        get("/nothing", 5.0),
        Err(FetchError::Http { code: 500, .. })
    ));
    assert_eq!(get("/junk", 5.0).unwrap_err(), FetchError::BadJson);
    // decode("utf-8", "replace"): a bad byte becomes U+FFFD, not a failure
    assert_eq!(
        get("/latin", 5.0).unwrap().body,
        json!({"name": "caf\u{fffd}"})
    );
    assert_eq!(get("/moved", 5.0).unwrap().body["a"], json!(1));
    let t0 = std::time::Instant::now();
    assert_eq!(get("/slow", 0.3).unwrap_err(), FetchError::Timeout);
    let took = t0.elapsed().as_secs_f64();
    assert!(took < 1.0, "a 0.3 s timeout took {took:.2} s");

    // Nothing listening: a port that was just free.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    match net.get(&format!("http://127.0.0.1:{port}/"), "scw-test", 5.0) {
        Err(FetchError::Connect(_)) => {}
        other => panic!("refused: {other:?}"),
    }
}
