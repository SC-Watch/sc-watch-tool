//! sc-watch's stored data, readable by the Python and the Rust versions
//! alike while one replaces the other: the JSON config files ([`json`], as
//! jsonstore.py keeps them). The SQLite databases follow.

pub mod json;
