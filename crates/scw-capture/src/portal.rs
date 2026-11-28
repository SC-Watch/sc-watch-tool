//! The desktop's screen-cast portal (org.freedesktop.portal.ScreenCast):
//! how a program on a Wayland desktop is allowed to see the screen. The
//! desktop asks the user which screen to share - KDE Plasma shows its own
//! dialog - and hands back a PipeWire stream of it.
//!
//! The answer is remembered: the portal returns a restore token, saved with
//! sc-watch's data, and the next start hands it back so the desktop shares
//! the same screen again without asking. A token is good for one use; each
//! start saves the new one. Revoking it (KDE: System Settings, the
//! application permissions) makes the dialog come back.
//!
//! The calls follow the portal's own description (data/org.freedesktop.
//! portal.ScreenCast.xml in xdg-desktop-portal): each answer comes back as
//! a Response signal on a request object whose path the caller names in
//! advance, so the signal is listened for before the call is made.

use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicU32, Ordering};

use zbus::blocking::{Connection, Proxy, proxy::Builder};
use zbus::proxy::CacheProperties;
use zbus::zvariant::{self, OwnedObjectPath, OwnedValue, Value};

const DEST: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const SCREEN_CAST: &str = "org.freedesktop.portal.ScreenCast";

/// Source types (AvailableSourceTypes).
pub const MONITOR: u32 = 1;
pub const WINDOW: u32 = 2;
/// persist_mode: until the user revokes it.
const PERSIST_UNTIL_REVOKED: u32 = 2;

/// A shared screen: its PipeWire stream, kept open with the session.
pub struct Grant {
    /// The PipeWire remote the stream is on (for [`crate::pipewire`]).
    pub fd: Option<OwnedFd>,
    /// The stream's node.
    pub node: u32,
    /// Its size on the desktop, when the portal says.
    pub size: Option<(i32, i32)>,
    /// Where it sits on the desktop, when the portal says.
    pub position: Option<(i32, i32)>,
    /// What was shared (MONITOR or WINDOW), when the portal says.
    pub source_type: Option<u32>,
    /// The token to share the same screen next time without asking.
    pub restore_token: Option<String>,
    /// The portal's version.
    pub version: u32,
    session: OwnedObjectPath,
    conn: Connection,
}

impl Drop for Grant {
    fn drop(&mut self) {
        // Ends the share; the desktop's "sharing your screen" marker goes.
        if let Ok(p) = proxy(
            &self.conn,
            self.session.as_str(),
            "org.freedesktop.portal.Session",
        ) {
            let _ = p.call_noreply("Close", &());
        }
    }
}

fn err(what: &str) -> impl Fn(zbus::Error) -> String + '_ {
    move |e| format!("{what}: {e}")
}

fn proxy<'a>(conn: &Connection, path: &'a str, iface: &'a str) -> Result<Proxy<'a>, zbus::Error> {
    Builder::new(conn)
        .destination(DEST)?
        .path(path)?
        .interface(iface)?
        .cache_properties(CacheProperties::No)
        .build()
}

static NEXT: AtomicU32 = AtomicU32::new(0);

/// A token unique in this process, for a request or session path.
fn token(kind: &str) -> String {
    format!(
        "scw_{kind}_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// The request path the portal will use for `token` (Request's documented
/// rule: the caller's unique name without ':' and with '.' as '_').
fn request_path(conn: &Connection, token: &str) -> Result<String, String> {
    let name = conn
        .unique_name()
        .ok_or("the session bus gave no name to this program")?;
    let sender = name.as_str().trim_start_matches(':').replace('.', "_");
    Ok(format!("{PATH}/request/{sender}/{token}"))
}

/// Makes the call `method` with `call` and waits for its Response: the
/// results, or why not.
fn request(
    conn: &Connection,
    method: &str,
    token: &str,
    call: impl FnOnce() -> zbus::Result<OwnedObjectPath>,
) -> Result<HashMap<String, OwnedValue>, String> {
    let path = request_path(conn, token)?;
    let req = proxy(conn, &path, "org.freedesktop.portal.Request").map_err(err(method))?;
    let mut responses = req.receive_signal("Response").map_err(err(method))?;
    let _handle = call().map_err(err(method))?;
    let msg = responses
        .next()
        .ok_or_else(|| format!("{method}: the portal went away"))?;
    let (code, results): (u32, HashMap<String, OwnedValue>) = msg
        .body()
        .deserialize()
        .map_err(|e| format!("{method}: an answer that could not be read: {e}"))?;
    match code {
        0 => Ok(results),
        1 => Err("the screen share was cancelled in the desktop's dialog".into()),
        _ => Err(format!("{method}: the desktop refused the screen share")),
    }
}

fn text(v: Option<&OwnedValue>) -> Option<String> {
    v.and_then(|v| String::try_from(v.try_clone().ok()?).ok())
}

/// One stream from Start's answer: its node, and its properties.
type Stream = (u32, HashMap<String, OwnedValue>);

/// Asks the desktop for a screen (`types`: [`MONITOR`], [`WINDOW`] or
/// both), handing back `restore` from last time so it need not ask again.
/// `say` is told before the desktop may show its dialog. Blocks until the
/// user answers.
pub fn share_screen(
    types: u32,
    restore: Option<&str>,
    say: &dyn Fn(&str),
) -> Result<Grant, String> {
    let conn = Connection::session()
        .map_err(|e| format!("no session bus to reach the desktop's screen-cast portal ({e})"))?;
    let portal = proxy(&conn, PATH, SCREEN_CAST).map_err(err("the screen-cast portal"))?;
    let version: u32 = portal.get_property("version").map_err(|e| {
        format!(
            "this desktop has no screen-cast portal ({e}) - on KDE Plasma it comes with \
             xdg-desktop-portal-kde"
        )
    })?;
    let available: u32 = portal
        .get_property("AvailableSourceTypes")
        .unwrap_or(MONITOR);
    let types = if types & available == 0 {
        MONITOR
    } else {
        types & available
    };

    let t = token("create");
    let mut opts: HashMap<&str, Value> = HashMap::new();
    opts.insert("handle_token", Value::from(t.as_str()));
    let session_token = token("session");
    opts.insert("session_handle_token", Value::from(session_token.as_str()));
    let results = request(&conn, "CreateSession", &t, || {
        portal.call("CreateSession", &(opts,))
    })?;
    let session =
        text(results.get("session_handle")).ok_or("CreateSession: no session in the answer")?;
    let session = OwnedObjectPath::try_from(session)
        .map_err(|e| format!("CreateSession: a session that is not a path: {e}"))?;

    let t = token("select");
    let mut opts: HashMap<&str, Value> = HashMap::new();
    opts.insert("handle_token", Value::from(t.as_str()));
    opts.insert("types", Value::from(types));
    opts.insert("multiple", Value::from(false));
    if version >= 4 {
        opts.insert("persist_mode", Value::from(PERSIST_UNTIL_REVOKED));
        if let Some(r) = restore.filter(|r| !r.is_empty()) {
            opts.insert("restore_token", Value::from(r));
        }
    }
    request(&conn, "SelectSources", &t, || {
        portal.call("SelectSources", &(&session, opts))
    })?;

    say(if restore.is_some() {
        "  screen: asking the desktop to share the same screen as last time"
    } else {
        "  screen: your desktop will ask which screen to share - pick the one the game is on"
    });
    let t = token("start");
    let mut opts: HashMap<&str, Value> = HashMap::new();
    opts.insert("handle_token", Value::from(t.as_str()));
    let mut results = request(&conn, "Start", &t, || {
        portal.call("Start", &(&session, "", opts))
    })?;
    let restore_token = text(results.get("restore_token"));
    let streams: Vec<Stream> = results
        .remove("streams")
        .ok_or("Start: no screen in the answer")
        .and_then(|v| {
            Vec::<Stream>::try_from(v).map_err(|_| "Start: screens that could not be read")
        })?;
    let (node, props) = streams
        .into_iter()
        .next()
        .ok_or("the desktop shared no screen")?;
    let pair = |k: &str| {
        props
            .get(k)
            .and_then(|v| <(i32, i32)>::try_from(v.try_clone().ok()?).ok())
    };
    let (size, position) = (pair("size"), pair("position"));
    let source_type = props
        .get("source_type")
        .and_then(|v| u32::try_from(v.try_clone().ok()?).ok());

    let empty: HashMap<&str, Value> = HashMap::new();
    let fd: zvariant::OwnedFd = portal
        .call("OpenPipeWireRemote", &(&session, empty))
        .map_err(err("OpenPipeWireRemote"))?;
    Ok(Grant {
        fd: Some(fd.into()),
        node,
        size,
        position,
        source_type,
        restore_token,
        version,
        session,
        conn,
    })
}
