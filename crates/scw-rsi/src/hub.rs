//! One lookup client for the whole program: the watcher's lookups and the
//! page's buttons share its pacing, its outage breaker and its remembered
//! answers. Rebuilt only when what it is built from changes - the lookup
//! mode, the source, the API key - read from settings.json at each ask, so
//! a change on the page reaches every user of it.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::{Cache, Client, Clock, Options, RequestLog, Transport, find_api_key};

/// Reads the environment (SC_API_KEY).
pub type Env = std::sync::Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Run on each cache the hub opens (the app watches it for changes).
pub type OnCache = Box<dyn Fn(&Cache) + Send + Sync>;

type Key = (String, String, Option<String>);

pub struct ClientHub {
    pub data: PathBuf,
    pub clock: Arc<dyn Clock>,
    pub transport: Arc<dyn Transport>,
    /// Names the program in SENTRY's request log.
    pub process: String,
    /// Stamp that log in UTC (tests).
    pub utc: bool,
    pub env: Env,
    pub on_cache: Option<OnCache>,
    current: Mutex<Option<(Key, Arc<Client>)>>,
}

impl ClientHub {
    pub fn new(
        data: PathBuf,
        clock: Arc<dyn Clock>,
        transport: Arc<dyn Transport>,
        process: &str,
        utc: bool,
        env: Env,
    ) -> Self {
        ClientHub {
            data,
            clock,
            transport,
            process: process.into(),
            utc,
            env,
            on_cache: None,
            current: Mutex::new(None),
        }
    }

    pub fn api_key(&self) -> Option<String> {
        find_api_key(
            (self.env)("SC_API_KEY").as_deref(),
            &self.data.join("sc_api_key.txt"),
        )
    }

    /// The client for the settings as they are now.
    pub fn get(&self) -> Result<Arc<Client>, String> {
        let cfg = scw_store::settings::load(&self.data.join("settings.json"));
        let text = |k: &str, d: &str| cfg.get(k).and_then(Value::as_str).unwrap_or(d).to_string();
        let key: Key = (
            text("rsi_mode", "auto"),
            text("rsi_source", "sentry"),
            self.api_key(),
        );
        let mut g = self.current.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((k, c)) = g.as_ref()
            && *k == key
        {
            return Ok(c.clone());
        }
        let _ = std::fs::create_dir_all(&self.data);
        let cache = Cache::open(&self.data.join("rsi_cache.db"), self.clock.clone())
            .map_err(|e| e.to_string())?;
        if let Some(f) = &self.on_cache {
            f(&cache);
        }
        let (a, b) = crate::SENTRY_LOG;
        let opts = Options {
            api_key: key.2.clone(),
            mode: key.0.clone(),
            sentry: key.1 != "sci",
            // The folder is made when the first line is written.
            log: Some(Arc::new(RequestLog {
                path: self.data.join(a).join(b),
                process: self.process.clone(),
                utc: self.utc,
            })),
            ..Options::default()
        };
        let c = Arc::new(Client::new(
            Arc::new(cache),
            self.transport.clone(),
            self.clock.clone(),
            opts,
        ));
        *g = Some((key, c.clone()));
        Ok(c)
    }
}
