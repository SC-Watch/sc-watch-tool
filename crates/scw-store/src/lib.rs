//! sc-watch's stored data, readable by the Python and the Rust versions
//! alike while one replaces the other: the JSON config files ([`json`], as
//! jsonstore.py keeps them) and the SQLite databases ([`db`] opens them as
//! dbopen.py does; [`reputation`] is sc-watch.db).

pub mod db;
pub mod json;
pub mod reputation;
