//! HTTP/1.0 as Python's http.server speaks it: one request a connection,
//! each connection on its own thread, the connection closed after the
//! answer. Errors are send_error's little HTML page.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::routes::{Reply, Request};
use crate::{MAX_BODY, Ui};

/// (phrase, explanation) of a status, as http.HTTPStatus has them.
pub fn status_text(code: u16) -> (&'static str, &'static str) {
    match code {
        200 => ("OK", "Request fulfilled, document follows"),
        400 => ("Bad Request", "Bad request syntax or unsupported method"),
        403 => (
            "Forbidden",
            "Request forbidden -- authorization will not help",
        ),
        404 => ("Not Found", "Nothing matches the given URI"),
        413 => (
            "Content Too Large",
            "Content larger than limits defined by server",
        ),
        415 => (
            "Unsupported Media Type",
            "Entity body in unsupported format",
        ),
        501 => ("Not Implemented", "Server does not support this operation"),
        _ => ("Internal Server Error", "Server got itself in trouble"),
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// The bytes of a reply that is not a stream.
pub fn render(reply: &Reply) -> Vec<u8> {
    match reply {
        Reply::Ok { ctype, body } => {
            let mut out = format!(
                "HTTP/1.0 200 OK\r\nServer: sc-watch\r\nContent-Type: {ctype}\r\n\
                 Content-Length: {}\r\nCache-Control: no-store\r\n\r\n",
                body.len()
            )
            .into_bytes();
            out.extend_from_slice(body);
            out
        }
        Reply::Error { code, message } => {
            let (phrase, explain) = status_text(*code);
            let msg = message.as_deref().unwrap_or(phrase);
            let body = format!(
                "<!DOCTYPE HTML>\n<html lang=\"en\">\n    <head>\n        <meta charset=\"utf-8\">\n        \
                 <title>Error response</title>\n    </head>\n    <body>\n        <h1>Error response</h1>\n        \
                 <p>Error code: {code}</p>\n        <p>Message: {}.</p>\n        \
                 <p>Error code explanation: {code} - {explain}.</p>\n    </body>\n</html>\n",
                html_escape(msg)
            );
            let mut out = format!(
                "HTTP/1.0 {code} {msg}\r\nServer: sc-watch\r\nConnection: close\r\n\
                 Content-Type: text/html;charset=utf-8\r\nContent-Length: {}\r\n\r\n",
                body.len()
            )
            .into_bytes();
            out.extend_from_slice(body.as_bytes());
            out
        }
        Reply::Stream { .. } => Vec::new(),
    }
}

/// Reads one request; None when the connection had nothing usable.
pub fn read_request(stream: &mut dyn Read) -> Option<Request> {
    let mut r = BufReader::new(stream);
    let mut line = String::new();
    r.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).ok()? == 0 {
            break;
        }
        let h = h.trim_end_matches(['\r', '\n']);
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let mut req = Request {
        method,
        target,
        headers,
        body: Vec::new(),
    };
    // The body is read only when it is a size the server would take.
    if let Some(n) = req
        .header("Content-Length")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n <= MAX_BODY)
    {
        let mut body = vec![0; n];
        r.read_exact(&mut body).ok()?;
        req.body = body;
    }
    Some(req)
}

fn serve_one(ui: &Arc<Ui>, mut stream: TcpStream) {
    let Some(req) = read_request(&mut stream) else {
        return;
    };
    let reply = ui.handle(&req);
    if let Reply::Stream { view, arg } = &reply {
        let head = "HTTP/1.0 200 OK\r\nServer: sc-watch\r\nContent-Type: text/event-stream\r\n\
                    Cache-Control: no-store\r\nConnection: keep-alive\r\nX-Accel-Buffering: no\r\n\r\n";
        if stream.write_all(head.as_bytes()).is_err() {
            return;
        }
        let keep = || !ui.shutting_down.load(Ordering::Relaxed);
        ui.stream(&mut stream, view.as_deref(), arg, &keep);
        return;
    }
    let _ = stream.write_all(&render(&reply));
    let _ = stream.flush();
}

/// Serves until the process ends: each connection on its own thread.
pub fn serve(ui: Arc<Ui>, listener: TcpListener) {
    for conn in listener.incoming() {
        if ui.shutting_down.load(Ordering::Relaxed) {
            break;
        }
        let Ok(stream) = conn else { continue };
        let ui = ui.clone();
        std::thread::spawn(move || serve_one(&ui, stream));
    }
}
