//! Lokale KI für die Mosaik-Apps: Gemma 4 über llama.cpp, nur auf 127.0.0.1.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod client;
pub mod config;
pub mod error;
pub mod http;
pub mod models;
pub mod server;
pub mod tier;

pub use config::{KiConfig, default_config_path, default_install_dir};
pub use error::KiError;
pub use tier::Tier;
