//! ONE request to SENTRY through the port's whole path - pacing, breaker,
//! HTTPS, the 404's lookup state, the request log - to see it works for
//! real. Database-only (scrape=false): SENTRY reads nothing from RSI for it
//! and it costs none of its allowance for new names. No API key, and a
//! throwaway cache and log in `<work folder>`, never the real ones.
//!
//!     sentry_probe <work folder> [made-up handle]

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use scw_rsi::{Answer, Cache, Client, Https, Options, RequestLog, SENTRY_TIMEOUT, SystemClock};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let work = Path::new(&args[1]);
    let handle = args.get(2).map(String::as_str).unwrap_or("EXMPLPROBE");
    std::fs::create_dir_all(work).unwrap();
    let clock = Arc::new(SystemClock);
    let cache = Arc::new(Cache::open(&work.join("rsi_cache.db"), clock.clone()).unwrap());
    let log_path = work.join("sentry_requests.log");
    let client = Client::new(
        cache,
        Arc::new(Https::new()),
        clock,
        Options {
            api_key: None,
            sentry: true,
            log: Some(Arc::new(RequestLog {
                path: log_path.clone(),
                process: "probe".into(),
                utc: false,
            })),
            ..Options::default()
        },
    );
    let sentry = client.sentry().unwrap();
    let t0 = Instant::now();
    let got = sentry.citizen(handle, false, SENTRY_TIMEOUT, None, false);
    let ms = t0.elapsed().as_millis();
    match &got {
        Ok(Answer::Nobody) => println!("answer: nobody (a 404 without not_found) in {ms} ms"),
        Ok(Answer::NotOnRsi) => {
            println!("answer: RSI has no such account (404 not_found) in {ms} ms")
        }
        Ok(Answer::Record(v)) => println!(
            "answer: a record in {ms} ms, {} top-level fields",
            v.as_object().map_or(0, |m| m.len())
        ),
        Err(e) => println!(
            "failed in {ms} ms: {} - {}",
            e.type_name(),
            client.describe(e)
        ),
    }
    println!(
        "SENTRY paused for {:.0} s; live lookups limited for {:.0} s",
        sentry.down_for(),
        sentry.waits().1
    );
    for line in std::fs::read_to_string(&log_path)
        .unwrap_or_default()
        .lines()
    {
        println!("log: {line}");
    }
}
