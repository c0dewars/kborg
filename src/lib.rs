#![forbid(unsafe_code)]
pub mod cli;
pub mod copilot;
pub mod gateway;
pub mod metrics;
pub mod policy;
pub mod safety;
pub mod themes;
pub mod tui;
pub mod watch;

pub type Result<T> = std::result::Result<T, String>;
