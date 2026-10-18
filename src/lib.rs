//! Lokale KI für die Mosaik-Apps: Gemma 4 über llama.cpp, nur auf 127.0.0.1.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod config;
pub mod error;
pub mod models;
pub mod tier;

pub use config::KiConfig;
pub use error::KiError;
pub use tier::Tier;
