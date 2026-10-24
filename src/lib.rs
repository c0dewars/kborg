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
pub mod workload_tail;

pub type Result<T> = std::result::Result<T, String>;

pub mod audit;
pub mod cluster_health;
pub mod completions;
pub mod config;
pub mod dashboard_health;
pub mod dashboard_memory;
pub mod edit;
pub mod exports;
pub mod external_editor;
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
pub mod archive;
pub mod auth;
pub mod bench;
pub mod debug_container;
pub mod extensions;
pub mod favorites;
pub mod file_copy;
pub mod hotkeys;
pub mod insights;
pub mod keymap;
pub mod lint;
pub mod manifest_apply;
pub mod navigation_views;
pub mod node_shell;
pub mod pager;
pub mod screendump;
pub mod secrets;
pub mod selections;
pub mod selectors;
pub mod session;
pub mod splash;
pub mod transport;
pub mod ui_logs;
pub mod ui_secrets;
pub mod ui_subjects;
pub mod views;
pub mod workload_actions;

pub mod helm;

pub mod helm_crds;
pub mod helm_extensions;
pub mod helm_lifecycle;
pub mod helm_migration;
pub mod helm_oci;
pub mod helm_provenance;
pub mod helm_sigstore;
