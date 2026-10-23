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

pub mod audit;
pub mod completions;
pub mod config;
pub mod edit;
pub mod exports;
pub mod inspect;
pub mod logs;
pub mod manifest_edit;
pub mod node_maintenance;
pub mod ops;
pub mod overview;
pub mod ui_yaml;

pub mod handoff;

pub mod advice;
pub mod aliases;
pub mod allocations;
pub mod auth;
pub mod extensions;
pub mod insights;
pub mod navigation_views;
pub mod pager;
pub mod screendump;
pub mod selectors;
pub mod session;
pub mod transport;
pub mod ui_logs;

pub mod helm;

pub mod helm_crds;
pub mod helm_extensions;
pub mod helm_lifecycle;
pub mod helm_migration;
pub mod helm_oci;
pub mod helm_provenance;
pub mod helm_sigstore;
