//! Voice Studio core: everything except the desktop UI.

pub mod audio;
pub mod catalog;
pub mod download;
pub mod engine;
pub mod export;
pub mod fsutil;
pub mod g2p;
pub mod markup;
pub mod profiles;
pub mod project;
pub mod pyenv;
pub mod queue;
pub mod render;
pub mod settings;
pub mod sysmon;

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
