//! Dashboard history: a bounded, local time series per context so the chart
//! panel (`G`, then `H`) can show the last 24 hours or 7 days after a restart.
//!
//! Only numbers are kept: Pods needing attention, container restarts, Node
//! CPU and memory percent, in 5-minute buckets for 7 days (at most 2,017
//! points, under 256 KiB a context). No object names, events, logs or
//! credentials. Files are private (`history/` in the config folder, owner
//! only), named by a hash of the context, and written at most once a minute.
//! `ui.yaml` `dashboard.history: false` stops reading and writing them.
use crate::{Result, safety};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

/// Seconds per stored point.
pub const BUCKET: i64 = 300;
/// How far back points are kept.
pub const KEEP: i64 = 7 * 24 * 3600;
const MAX_POINTS: usize = (KEEP / BUCKET) as usize + 1;
const MAX_BYTES: u64 = 256 * 1024;
/// Seconds between writes of one context's file.
const SAVE_EVERY: i64 = 60;

/// One bucket. Attention, CPU and memory are the bucket's highest sample;
/// restarts are the restarts counted in it. `None` is unknown, never zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Point {
    /// Unix seconds at the start of the bucket.
    pub t: i64,
    pub attention: u64,
    pub restarts: Option<u64>,
    pub cpu: Option<u64>,
    pub memory: Option<u64>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    schema_version: u32,
    context: String,
    points: Vec<Point>,
}

/// The saved series of one context.
pub struct History {
    context: String,
    path: Option<PathBuf>,
    points: VecDeque<Point>,
    saved_at: Option<i64>,
    dirty: bool,
}

/// The history folder, or `None` when the config folder is unavailable.
pub fn folder() -> Option<PathBuf> {
    // Unit tests never read or write the developer's own history.
    if cfg!(test) {
        return None;
    }
    safety::config_dir().ok().map(|root| root.join("history"))
}

fn file_name(context: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, context.as_bytes());
    let hex: String = digest.as_ref()[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{hex}.json")
}

fn bucket(t: i64) -> i64 {
    t - t.rem_euclid(BUCKET)
}

fn max(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

impl History {
    /// A series that is never saved (demo data, history switched off).
    pub fn memory(context: &str) -> Self {
        Self {
            context: context.into(),
            path: None,
            points: VecDeque::new(),
            saved_at: None,
            dirty: false,
        }
    }
    /// Read the context's saved series from `folder`. A missing, unreadable
    /// or foreign file starts an empty series, which the next save replaces.
    pub fn load(folder: &Path, context: &str, now: i64) -> Self {
        let path = folder.join(file_name(context));
        let mut history = Self {
            path: Some(path.clone()),
            ..Self::memory(context)
        };
        let Ok(bytes) = safety::read_small(&path, MAX_BYTES) else {
            return history;
        };
        let Ok(file) = serde_json::from_slice::<File>(&bytes) else {
            return history;
        };
        if file.schema_version != 1 || file.context != context {
            return history;
        }
        let mut points: Vec<Point> = file
            .points
            .into_iter()
            .filter(|point| point.t <= now && point.t > now - KEEP && point.t % BUCKET == 0)
            .collect();
        points.sort_by_key(|point| point.t);
        points.dedup_by_key(|point| point.t);
        let skip = points.len().saturating_sub(MAX_POINTS);
        history.points = points.into_iter().skip(skip).collect();
        history
    }
    /// Add one dashboard sample taken at Unix time `now`.
    pub fn record(
        &mut self,
        now: i64,
        attention: u64,
        restarts: Option<u64>,
        cpu: Option<u64>,
        memory: Option<u64>,
    ) {
        let t = bucket(now);
        match self.points.back_mut() {
            Some(last) if last.t == t => {
                last.attention = last.attention.max(attention);
                last.restarts = match (last.restarts, restarts) {
                    (Some(a), Some(b)) => Some(a.saturating_add(b)),
                    (a, b) => a.or(b),
                };
                last.cpu = max(last.cpu, cpu);
                last.memory = max(last.memory, memory);
            }
            Some(last) if last.t > t => return,
            _ => self.points.push_back(Point {
                t,
                attention,
                restarts,
                cpu,
                memory,
            }),
        }
        while self
            .points
            .front()
            .is_some_and(|point| point.t <= now - KEEP)
            || self.points.len() > MAX_POINTS
        {
            self.points.pop_front();
        }
        self.dirty = true;
    }
    /// Write the series when it changed and the last write is a minute old.
    pub fn save_if_due(&mut self, now: i64) -> Result<()> {
        if !self.dirty || self.saved_at.is_some_and(|saved| now - saved < SAVE_EVERY) {
            return Ok(());
        }
        let Some(path) = &self.path else {
            return Ok(());
        };
        let file = File {
            schema_version: 1,
            context: self.context.clone(),
            points: self.points.iter().copied().collect(),
        };
        let bytes = serde_json::to_vec(&file).map_err(|_| "cannot encode dashboard history")?;
        safety::atomic_write(path, &bytes)?;
        self.saved_at = Some(now);
        self.dirty = false;
        Ok(())
    }
    /// Points from the last `span` seconds, oldest first.
    pub fn points(&self, now: i64, span: i64) -> Vec<Point> {
        self.points
            .iter()
            .filter(|point| point.t > now - span - BUCKET)
            .copied()
            .collect()
    }
    pub fn saves(&self) -> bool {
        self.path.is_some()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn folder() -> Result<crate::config_watch::tests::Folder> {
        crate::config_watch::tests::folder()
    }

    #[test]
    fn samples_fold_into_five_minute_buckets_and_survive_a_restart() -> Result<()> {
        let dir = folder()?;
        let day = 1_800_000_000 - 1_800_000_000 % BUCKET;
        let mut history = History::load(&dir.0, "prod", day);
        history.record(day, 2, Some(1), Some(40), None);
        history.record(day + 15, 5, Some(3), Some(35), Some(60));
        history.record(day + BUCKET, 1, None, None, None);
        history.save_if_due(day + BUCKET)?;
        // Reopened a day later: yesterday's restarts are still there.
        let reopened = History::load(&dir.0, "prod", day + 24 * 3600);
        let points = reopened.points(day + 24 * 3600, KEEP);
        assert_eq!(points.len(), 2);
        assert_eq!(
            points[0],
            Point {
                t: day,
                attention: 5,
                restarts: Some(4),
                cpu: Some(40),
                memory: Some(60)
            }
        );
        assert_eq!(points[1].restarts, None, "unknown stays unknown");
        // Another context's file is separate; the name is hashed.
        assert!(
            History::load(&dir.0, "staging", day)
                .points(day, KEEP)
                .is_empty()
        );
        let names: Vec<_> = std::fs::read_dir(&dir.0)
            .map_err(|_| "read")?
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().all(|name| !name.contains("prod")), "{names:?}");
        Ok(())
    }

    #[test]
    fn history_is_bounded_to_seven_days_and_written_at_most_once_a_minute() -> Result<()> {
        let dir = folder()?;
        let start = 1_800_000_000 - 1_800_000_000 % BUCKET;
        let mut history = History::load(&dir.0, "big", start);
        for step in 0..(MAX_POINTS as i64 + 500) {
            history.record(start + step * BUCKET, 1, Some(0), None, None);
        }
        let end = start + (MAX_POINTS as i64 + 499) * BUCKET;
        let points = history.points(end, KEEP);
        assert!(points.len() <= MAX_POINTS);
        assert!(points.first().is_some_and(|p| p.t > end - KEEP));
        history.save_if_due(end)?;
        let bytes = std::fs::metadata(dir.0.join(file_name("big")))
            .map_err(|_| "stat")?
            .len();
        assert!(bytes < MAX_BYTES, "{bytes}");
        // A second save within the minute is skipped.
        history.record(end + 1, 9, None, None, None);
        history.save_if_due(end + 30)?;
        assert_ne!(
            History::load(&dir.0, "big", end + 30)
                .points(end + 30, BUCKET)
                .last()
                .map(|p| p.attention),
            Some(9)
        );
        history.save_if_due(end + 61)?;
        assert_eq!(
            History::load(&dir.0, "big", end + 61)
                .points(end + 61, BUCKET)
                .last()
                .map(|p| p.attention),
            Some(9)
        );
        Ok(())
    }

    #[test]
    fn damaged_or_foreign_files_start_empty() -> Result<()> {
        let dir = folder()?;
        let now = 1_800_000_000;
        for text in [
            "not json",
            r#"{"schema_version":2,"context":"x","points":[]}"#,
            r#"{"schema_version":1,"context":"other","points":[{"t":1799999700,"attention":1,"restarts":null,"cpu":null,"memory":null}]}"#,
        ] {
            let path = dir.0.join(file_name("x"));
            let _ = std::fs::remove_file(&path);
            safety::write_new(&path, text.as_bytes())?;
            assert!(
                History::load(&dir.0, "x", now).points(now, KEEP).is_empty(),
                "{text}"
            );
        }
        // A series that never saves writes nothing.
        let mut memory = History::memory("demo");
        memory.record(now, 1, None, None, None);
        memory.save_if_due(now)?;
        assert!(!memory.saves());
        Ok(())
    }
}
