//! Pull-driven, time-bounded log follow with a rolling display and flood guards.
use std::time::Duration;

mod budget;
mod dynamic;
mod feed;
mod identity;
mod membership;
mod merged;
mod options;
mod rolling;
#[cfg(test)]
mod tests;

pub use dynamic::{DynamicLogFeed, MEMBERSHIP_REFRESH_INTERVAL, WorkloadLogFeed};
pub use feed::LogFeed;
pub(crate) use identity::{
    IdentityGuard, ordinary_container_names, validate_merge_target, verify_uid,
};
pub use membership::{LogMembership, LogPod, PodLogSelection};
pub(crate) use membership::{LogParent, log_pod, membership_bytes, owned_by, selected_pods};
pub use merged::MergedLogFeed;
pub use options::{FollowOptions, ReadOptions};
pub(crate) use rolling::snapshot_text;
pub use rolling::{RollingLog, without_timestamp};

pub const MAX_LOG_BYTES: usize = 256 * 1024;
pub const MAX_LINE_BYTES: usize = 4096;
pub const MAX_LOG_LINES: usize = 2000;
pub const DEFAULT_FOLLOW_SECONDS: u32 = 3600;
pub const MAX_FOLLOW_SECONDS: u32 = 86_400;
const MAX_RATE_BYTES: usize = 1024 * 1024;
const MAX_RATE_LINES: usize = 5000;
const MAX_RECONNECTS: u32 = 5;
const CONNECT_BUDGET: Duration = Duration::from_secs(15);
/// Pod-wide logging deliberately excludes init and ephemeral containers.
pub const MAX_MERGED_CONTAINERS: usize = 16;
pub const MAX_LOG_PODS: usize = 8;
pub const MAX_WORKLOAD_LOG_SOURCES: usize = 32;
pub(crate) const MAX_LOG_JOBS: usize = 64;
const MERGED_OPEN_CONCURRENCY: usize = 4;
const MEMBERSHIP_CHECK_INTERVAL: Duration = Duration::from_secs(5);
const MEMBERSHIP_CHECK_BUDGET: Duration = Duration::from_secs(3);
const MEMBERSHIP_BYTES: usize = 8 * 1024 * 1024;
