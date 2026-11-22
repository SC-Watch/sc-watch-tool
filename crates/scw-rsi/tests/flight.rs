//! The same question asked twice at once is answered once: a second lookup
//! of a handle already in flight waits for the first's answer instead of
//! sending its own request.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use common::FakeClock;
use scw_rsi::{Cache, Client, FetchError, Headers, Options, Reply, Transport};

/// Holds every request until told to answer it (404, not on RSI).
struct Gate {
    count: AtomicUsize,
    entered: Mutex<Sender<()>>,
    release: Mutex<Receiver<()>>,
}

impl Transport for Gate {
    fn get(&self, _url: &str, _ua: &str, _timeout: f64) -> Result<Reply, FetchError> {
        self.count.fetch_add(1, Ordering::SeqCst);
        let _ = self.entered.lock().unwrap().send(());
        let _ = self.release.lock().unwrap().recv();
        Err(FetchError::Http {
            code: 404,
            headers: Headers(vec![("X-Lookup-State".into(), "not_found".into())]),
        })
    }
}

#[test]
fn a_question_in_flight_is_not_asked_again() {
    let dir = std::env::temp_dir().join(format!("scw-flight-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let clock = Arc::new(FakeClock(Mutex::new((1_790_000_000.0, 5000.0))));
    let (entered_tx, entered) = channel();
    let (release, release_rx) = channel();
    let gate = Arc::new(Gate {
        count: AtomicUsize::new(0),
        entered: Mutex::new(entered_tx),
        release: Mutex::new(release_rx),
    });
    let cache = Arc::new(Cache::open(&dir.join("rsi_cache.db"), clock.clone()).unwrap());
    let client = Arc::new(Client::new(cache, gate.clone(), clock, Options::default()));

    let c1 = client.clone();
    let first = std::thread::spawn(move || c1.citizen("EXMPLONE", true, true));
    entered.recv().unwrap(); // the first is out on the network now
    let c2 = client.clone();
    let second = std::thread::spawn(move || c2.citizen("exmplone", true, true));
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(gate.count.load(Ordering::SeqCst), 1, "the second waited");
    release.send(()).unwrap();
    let (a, b) = (first.join().unwrap(), second.join().unwrap());
    assert_eq!(a, b, "both have the one answer");
    assert!(a.is_ok_and(|c| !c.exists));
    assert_eq!(gate.count.load(Ordering::SeqCst), 1);

    // Answered, the question can be asked anew.
    let c3 = client.clone();
    let third = std::thread::spawn(move || c3.citizen("EXMPLONE", true, true));
    entered.recv().unwrap();
    release.send(()).unwrap();
    assert!(third.join().unwrap().is_ok());
    assert_eq!(gate.count.load(Ordering::SeqCst), 2);
    let _ = std::fs::remove_dir_all(&dir);
}
