//! The last dashboard's contexts, namespace and tail picks, for the explicit
//! `kborg dashboard` launch (k10s-style memory). Only names are stored: no
//! credentials, UIDs, cluster data or approvals. A normal launch never reads it.
use crate::{Result, safety};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const FILE: &str = "last-dashboard.json";
const MAX_BYTES: u64 = 16 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TailPick {
    pub context: String,
    pub resource: String,
    pub namespace: String,
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LastDashboard {
    pub schema_version: u32,
    pub contexts: Vec<String>,
    pub namespace: String,
    #[serde(default)]
    pub tail: Vec<TailPick>,
}

impl LastDashboard {
    pub fn new(contexts: Vec<String>, namespace: String) -> Self {
        Self {
            schema_version: 1,
            contexts,
            namespace,
            tail: Vec::new(),
        }
    }
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1 {
            return Err("unsupported last-dashboard schema".into());
        }
        crate::overview::validate(&self.contexts)?;
        if self.contexts.len() > crate::overview::MAX_LIVE_CONTEXTS {
            return Err("last dashboard has too many contexts".into());
        }
        if self.namespace != "*" && !safety::valid_id(&self.namespace) {
            return Err("last dashboard namespace is invalid".into());
        }
        if self.tail.len() > crate::workload_tail::MAX_SOURCES
            || self.tail.iter().any(|pick| {
                !self.contexts.contains(&pick.context)
                    || !["deployments", "statefulsets"].contains(&pick.resource.as_str())
                    || !safety::valid_id(&pick.namespace)
                    || pick.name.is_empty()
                    || pick.name.len() > 253
                    || !pick.name.split('.').all(safety::valid_id)
            })
        {
            return Err("last dashboard tail selection is invalid".into());
        }
        Ok(())
    }
    pub fn picks(&self, workload: &crate::workload_tail::WorkloadRef) -> bool {
        self.tail.iter().any(|pick| {
            pick.context == workload.context
                && pick.resource == workload.resource
                && pick.namespace == workload.namespace
                && pick.name == workload.name
        })
    }
}

fn path() -> Result<PathBuf> {
    Ok(safety::config_dir()?.join(FILE))
}

fn load_at(path: &Path) -> Result<Option<LastDashboard>> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = safety::read_small(path, MAX_BYTES)?;
    let last: LastDashboard = serde_json::from_slice(&bytes)
        .map_err(|_| "last dashboard file is not valid schema 1 JSON")?;
    last.validate()?;
    Ok(Some(last))
}

fn save_at(path: &Path, last: &LastDashboard) -> Result<()> {
    last.validate()?;
    let bytes = serde_json::to_vec_pretty(last).map_err(|_| "cannot encode last dashboard")?;
    safety::atomic_write(path, &bytes)
}

pub fn load() -> Result<Option<LastDashboard>> {
    load_at(&path()?)
}

/// Records the dashboard scope, keeping tail picks that still belong to it.
pub fn remember_scope(contexts: &[String], namespace: &str) -> Result<()> {
    if cfg!(test) {
        return Ok(());
    }
    let mut last = load().ok().flatten().unwrap_or_default();
    let mut next = LastDashboard::new(contexts.to_vec(), namespace.into());
    next.tail = std::mem::take(&mut last.tail)
        .into_iter()
        .filter(|pick| contexts.contains(&pick.context))
        .collect();
    save_at(&path()?, &next)
}

pub fn remember_tail(workloads: &[crate::workload_tail::WorkloadRef]) -> Result<()> {
    if cfg!(test) {
        return Ok(());
    }
    let Some(mut last) = load()? else {
        return Ok(());
    };
    last.tail = workloads
        .iter()
        .filter(|w| last.contexts.contains(&w.context))
        .map(|w| TailPick {
            context: w.context.clone(),
            resource: w.resource.clone(),
            namespace: w.namespace.clone(),
            name: w.name.clone(),
        })
        .collect();
    save_at(&path()?, &last)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn memory_round_trips_names_only_and_rejects_invalid_or_unknown_fields() -> Result<()> {
        let root = std::env::temp_dir()
            .canonicalize()
            .map_err(|_| "temp")?
            .join(format!("kborg-memory-test-{}", std::process::id()));
        safety::ensure_private_dir(&root)?;
        let file = root.join(FILE);
        let mut last = LastDashboard::new(vec!["east".into(), "west".into()], "shop".into());
        last.tail.push(TailPick {
            context: "west".into(),
            resource: "deployments".into(),
            namespace: "shop".into(),
            name: "api".into(),
        });
        save_at(&file, &last)?;
        assert_eq!(load_at(&file)?, Some(last.clone()));
        let workload = crate::workload_tail::WorkloadRef {
            context: "west".into(),
            resource: "deployments".into(),
            namespace: "shop".into(),
            name: "api".into(),
            uid: "fresh-uid".into(),
        };
        assert!(last.picks(&workload));
        let mut bad = last.clone();
        bad.tail[0].context = "elsewhere".into();
        assert!(save_at(&file, &bad).is_err());
        let mut bad = last;
        bad.namespace = "Bad/NS".into();
        assert!(save_at(&file, &bad).is_err());
        std::fs::write(
            &file,
            br#"{"schema_version":1,"contexts":["a"],"namespace":"x","token":"t"}"#,
        )
        .map_err(|_| "write")?;
        assert!(load_at(&file).is_err());
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }
}
