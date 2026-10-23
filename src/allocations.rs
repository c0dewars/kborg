//! Optional comparisons against a separately read, current resource snapshot.
//! Pod containers are compared individually; init/ephemeral containers are never
//! aggregated or guessed. Declared configuration can change after a measurement.
use crate::{
    Result,
    metrics::{self, MetricsScope, MetricsSnapshot},
    safety,
};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashSet},
    io::Write,
};

const MAX_OBJECTS: usize = 1000;
const MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Serialize)]
pub struct ResourceComparison {
    pub generation_verified: bool,
    pub status: String,
    pub bases: Vec<BasisComparison>,
}
#[derive(Debug, Serialize)]
pub struct BasisComparison {
    pub basis: &'static str,
    pub cpu: QuantityComparison,
    pub memory: QuantityComparison,
}
#[derive(Debug, Serialize)]
pub struct QuantityComparison {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount: Option<u64>,
    pub unit: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rounded_up: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_percent: Option<Percent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<&'static str>,
}
#[derive(Debug, Serialize)]
pub struct Percent {
    /// Percentage rounded to two decimal places with checked integer arithmetic.
    pub value: String,
    pub rounded: bool,
}
impl ResourceComparison {
    pub(crate) fn text(&self) -> String {
        let mut output = format!("  Resource basis: {}\n", self.status);
        for basis in &self.bases {
            output.push_str(&format!(
                "  {} | CPU {} | MEMORY {}\n",
                basis.basis,
                basis.cpu.text(true),
                basis.memory.text(false)
            ));
        }
        output
    }
}
impl QuantityComparison {
    fn text(&self, cpu: bool) -> String {
        let quantity = self
            .amount
            .map(|amount| {
                if cpu {
                    format!("{}m", metrics::format_millicores(amount))
                } else {
                    metrics::format_memory(amount)
                }
            })
            .map(|quantity| {
                format!(
                    "{quantity}{}",
                    if self.rounded_up == Some(true) {
                        " (rounded up)"
                    } else {
                        ""
                    }
                )
            });
        match (&quantity, &self.usage_percent) {
            (Some(quantity), Some(percent)) => format!(
                "{quantity}; usage {}{}%",
                if percent.rounded { "~" } else { "" },
                percent.value
            ),
            (Some(quantity), None) => format!(
                "{quantity}; {}",
                self.unavailable.unwrap_or("comparison unavailable")
            ),
            _ => self.unavailable.unwrap_or("not declared").into(),
        }
    }
}

struct ByteBudget(usize);
impl Write for ByteBudget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_BYTES.saturating_sub(self.0) {
            return Err(std::io::ErrorKind::FileTooLarge.into());
        }
        self.0 += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Enrich an already valid metrics snapshot from a bounded Gateway pods/nodes
/// list. Invalid resource projections fail independently of the usage read.
pub fn enrich(snapshot: &mut MetricsSnapshot, objects: &Value) -> Result<()> {
    for row in &mut snapshot.items {
        row.allocations = None;
    }
    snapshot.allocation_status = None;
    let items = objects["items"]
        .as_array()
        .ok_or("resource denominator list is malformed")?;
    if items.len() > MAX_OBJECTS {
        return Err("resource denominator list exceeds 1000 objects".into());
    }
    serde_json::to_writer(&mut ByteBudget(0), objects)
        .map_err(|_| "resource denominator list exceeds 8 MiB or cannot be encoded")?;
    let mut index = BTreeMap::new();
    let scope = snapshot.scope;
    let selected_ns = snapshot.namespace.as_deref().unwrap_or("*");
    if selected_ns != "*" && !safety::valid_id(selected_ns) {
        return Err("invalid denominator namespace selection".into());
    }
    for object in items {
        if object["kind"].as_str().is_some_and(|kind| {
            kind != if scope == MetricsScope::Pods {
                "Pod"
            } else {
                "Node"
            }
        }) {
            return Err("resource denominator list has an unexpected kind".into());
        }
        let name = object["metadata"]["name"]
            .as_str()
            .ok_or("resource denominator identity missing")?;
        if !valid_name(name) {
            return Err("resource denominator identity is invalid".into());
        }
        let namespace = if scope == MetricsScope::Pods {
            let ns = object["metadata"]["namespace"]
                .as_str()
                .ok_or("pod denominator namespace missing")?;
            if !safety::valid_id(ns) || (selected_ns != "*" && selected_ns != ns) {
                return Err("resource denominator is outside selected namespace".into());
            }
            Some(ns)
        } else {
            if !object["metadata"]["namespace"].is_null() {
                return Err("node denominator must be cluster scoped".into());
            }
            None
        };
        if index.insert((namespace, name), object).is_some() {
            return Err("duplicate resource denominator identity".into());
        }
    }
    for row in &mut snapshot.items {
        let object = index
            .get(&(row.namespace.as_deref(), row.name.as_str()))
            .copied();
        row.allocations = Some(compare(row, object, scope));
    }
    snapshot.allocation_status = Some(format!(
        "Explicit {} resource snapshot: {} objects{}. Percentages compare window usage with current declared configuration; rounded values use '~' (two decimal places).",
        if scope == MetricsScope::Pods {
            "pod"
        } else {
            "node"
        },
        items.len(),
        if objects["truncated"] == true {
            "; bounded/truncated, unmatched resources unavailable"
        } else {
            ""
        },
    ));
    Ok(())
}
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.')
        })
}
fn unavailable(status: &str) -> ResourceComparison {
    ResourceComparison {
        generation_verified: false,
        status: status.into(),
        bases: Vec::new(),
    }
}
fn compare(
    row: &metrics::MetricsRow,
    object: Option<&Value>,
    scope: MetricsScope,
) -> ResourceComparison {
    let Some(object) = object else {
        return unavailable("resource missing from bounded snapshot; no denominator inferred");
    };
    let object_uid = object["metadata"]["uid"]
        .as_str()
        .filter(|uid| !uid.is_empty());
    let generation_verified = match (row.identity_uid.as_deref(), object_uid) {
        (Some(measured), Some(current)) if measured != current => {
            return unavailable("resource UID changed; name was replaced, comparison suppressed");
        }
        (Some(_), Some(_)) => true,
        _ => false,
    };
    let measured = metrics::timestamp_key(&row.timestamp);
    let created = object["metadata"]["creationTimestamp"]
        .as_str()
        .and_then(metrics::timestamp_key);
    match (measured, created) {
        (Some(measured), Some(created)) if created > measured => {
            return unavailable("resource created after this measurement; comparison suppressed");
        }
        (None, _) | (_, None) => {
            return unavailable(
                "measurement/resource creation timestamp unavailable; comparison suppressed",
            );
        }
        _ => {}
    }
    let usage = (row.cpu_nanocores, row.memory_bytes);
    let bases = match scope {
        MetricsScope::Pods => {
            let Some(containers) = object["spec"]["containers"].as_array() else {
                return unavailable("ordinary pod container specification unavailable");
            };
            if containers.len() > MAX_OBJECTS {
                return unavailable("pod container specification exceeds bound");
            }
            let mut names = HashSet::new();
            for container in containers {
                let Some(name) = container["name"]
                    .as_str()
                    .filter(|name| safety::valid_id(name))
                else {
                    return unavailable("pod container identity is invalid");
                };
                if !names.insert(name) {
                    return unavailable("pod has duplicate container identities");
                }
            }
            let container = containers
                .iter()
                .find(|container| container["name"].as_str() == row.container.as_deref());
            let Some(container) = container else {
                return unavailable(
                    "measurement is not an ordinary pod container; no init/ephemeral aggregation",
                );
            };
            ["requests", "limits"]
                .into_iter()
                .map(|basis| basis_comparison(basis, &container["resources"][basis], usage))
                .collect()
        }
        MetricsScope::Nodes => ["allocatable", "capacity"]
            .into_iter()
            .map(|basis| basis_comparison(basis, &object["status"][basis], usage))
            .collect(),
    };
    ResourceComparison {
        generation_verified,
        status: if generation_verified {
            "UID matched; current resource configuration snapshot".into()
        } else if row.identity_uid.is_none() {
            "name-based snapshot; Metrics API omitted UID; creation precedes measurement, generation unverified".into()
        } else {
            "name-based snapshot; resource UID unavailable; generation unverified".into()
        },
        bases,
    }
}
fn basis_comparison(basis: &'static str, values: &Value, usage: (u64, u64)) -> BasisComparison {
    BasisComparison {
        basis,
        cpu: quantity_comparison(&values["cpu"], usage.0, true),
        memory: quantity_comparison(&values["memory"], usage.1, false),
    }
}
fn quantity_comparison(value: &Value, usage: u64, cpu: bool) -> QuantityComparison {
    let unit = if cpu { "nanocores" } else { "bytes" };
    let absent = |reason| QuantityComparison {
        raw: None,
        amount: None,
        unit,
        rounded_up: None,
        usage_percent: None,
        unavailable: Some(reason),
    };
    if value.is_null() {
        return absent("not declared");
    }
    let Some(raw) = value.as_str() else {
        return absent("invalid quantity; denominator unavailable");
    };
    let normalized = if cpu {
        metrics::normalize_cpu_quantity(raw)
    } else {
        metrics::normalize_memory_quantity(raw)
    };
    let Ok((amount, rounded_up)) = normalized else {
        return absent("invalid/overflowing quantity; denominator unavailable");
    };
    QuantityComparison {
        raw: Some(raw.into()),
        amount: Some(amount),
        unit,
        rounded_up: Some(rounded_up),
        usage_percent: percentage(usage, amount),
        unavailable: (amount == 0).then_some("zero denominator; no percentage"),
    }
}
fn percentage(usage: u64, denominator: u64) -> Option<Percent> {
    if denominator == 0 {
        return None;
    }
    let numerator = u128::from(usage).checked_mul(10_000)?;
    let denominator = u128::from(denominator);
    let rounded = numerator % denominator != 0;
    let scaled = numerator.checked_add(denominator / 2)? / denominator;
    Some(Percent {
        value: format!("{}.{:02}", scaled / 100, scaled % 100),
        rounded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pod(requests: Value, limits: Value) -> Value {
        json!({"kind":"Pod","metadata":{"name":"api-7d4f9","namespace":"demo","uid":"demo-Pod-api-7d4f9","creationTimestamp":"2026-10-10T07:00:00Z"},"spec":{"containers":[{"name":"app","resources":{"requests":requests,"limits":limits}}],"initContainers":[{"name":"init","resources":{"requests":{"cpu":"10","memory":"1Ti"}}}]}})
    }
    fn list(object: Value) -> Value {
        json!({"items":[object],"truncated":false})
    }
    fn allocations(snapshot: &MetricsSnapshot) -> &ResourceComparison {
        snapshot.items[0]
            .allocations
            .as_ref()
            .unwrap_or_else(|| panic!("comparison missing"))
    }
    #[test]
    fn pod_container_comparisons_have_exact_bases_percentages_and_no_init_aggregation() -> Result<()>
    {
        let mut snapshot = metrics::demo(MetricsScope::Pods, "demo")?;
        enrich(
            &mut snapshot,
            &list(pod(
                json!({"cpu":"250m","memory":"128Mi"}),
                json!({"cpu":"1","memory":"512Mi"}),
            )),
        )?;
        let comparison = allocations(&snapshot);
        assert!(comparison.generation_verified);
        assert_eq!(comparison.bases.len(), 2);
        assert_eq!(comparison.bases[0].cpu.amount, Some(250_000_000));
        assert_eq!(
            comparison.bases[0]
                .cpu
                .usage_percent
                .as_ref()
                .map(|percent| percent.value.as_str()),
            Some("50.00")
        );
        assert_eq!(
            comparison.bases[0]
                .memory
                .usage_percent
                .as_ref()
                .map(|percent| percent.value.as_str()),
            Some("50.00")
        );
        assert_eq!(
            comparison.bases[1]
                .cpu
                .usage_percent
                .as_ref()
                .map(|percent| percent.value.as_str()),
            Some("12.50")
        );
        assert_eq!(
            comparison.bases[1]
                .memory
                .usage_percent
                .as_ref()
                .map(|percent| percent.value.as_str()),
            Some("12.50")
        );
        assert!(
            snapshot
                .text()
                .contains("requests | CPU 250m; usage 50.00%")
        );
        assert!(!snapshot.text().contains("1Ti"));
        snapshot.items[0].container = Some("init".into());
        enrich(&mut snapshot, &list(pod(json!({"cpu":"250m"}), json!({}))))?;
        assert!(allocations(&snapshot).bases.is_empty());
        assert!(allocations(&snapshot).status.contains("not an ordinary"));
        Ok(())
    }
    #[test]
    fn absent_zero_malformed_and_overflowing_denominators_do_not_create_percentages() -> Result<()>
    {
        let mut snapshot = metrics::demo(MetricsScope::Pods, "demo")?;
        enrich(
            &mut snapshot,
            &list(pod(json!({}), json!({"cpu":"0","memory":"0"}))),
        )?;
        let comparison = allocations(&snapshot);
        assert!(
            comparison.bases[0].cpu.amount.is_none()
                && comparison.bases[0].cpu.usage_percent.is_none()
        );
        assert_eq!(comparison.bases[1].memory.amount, Some(0));
        assert!(comparison.bases[1].memory.usage_percent.is_none());
        assert!(snapshot.text().contains("zero denominator; no percentage"));
        enrich(
            &mut snapshot,
            &list(pod(
                json!({"cpu":"CANARY\u{1b}","memory":"18446744073709551616"}),
                json!({"cpu":42,"memory":"-1"}),
            )),
        )?;
        assert!(
            allocations(&snapshot)
                .bases
                .iter()
                .all(|basis| basis.cpu.amount.is_none() && basis.memory.amount.is_none())
        );
        assert!(!snapshot.text().contains("CANARY") && !snapshot.text().contains('\u{1b}'));
        assert_eq!(snapshot.items[0].cpu_nanocores, 125_000_000);
        assert_eq!(snapshot.items[0].memory_bytes, 67_108_864);
        assert_eq!(
            percentage(u64::MAX, 1).map(|percent| percent.value),
            Some("1844674407370955161500.00".into())
        );
        assert_eq!(
            percentage(u64::MAX, u64::MAX).map(|percent| percent.value),
            Some("100.00".into())
        );
        assert_eq!(
            percentage(1, 3).map(|percent| (percent.value, percent.rounded)),
            Some(("33.33".into(), true))
        );
        assert_eq!(
            percentage(1, 6).map(|percent| percent.value),
            Some("16.67".into())
        );
        assert_eq!(
            percentage(125_000_000, 10_000_000).map(|percent| percent.value),
            Some("1250.00".into())
        );
        Ok(())
    }
    #[test]
    fn name_only_provider_results_are_qualified_and_uid_or_creation_replacements_are_suppressed()
    -> Result<()> {
        let object = pod(json!({"cpu":"250m","memory":"128Mi"}), json!({}));
        let mut snapshot = metrics::demo(MetricsScope::Pods, "demo")?;
        snapshot.items[0].identity_uid = None;
        enrich(&mut snapshot, &list(object.clone()))?;
        assert!(!allocations(&snapshot).generation_verified);
        assert!(
            allocations(&snapshot)
                .status
                .contains("Metrics API omitted UID")
        );
        assert_eq!(
            allocations(&snapshot).bases[0]
                .cpu
                .usage_percent
                .as_ref()
                .map(|percent| percent.value.as_str()),
            Some("50.00")
        );
        let mut recreated = object.clone();
        recreated["metadata"]["creationTimestamp"] = "2026-10-10T07:01:00Z".into();
        enrich(&mut snapshot, &list(recreated))?;
        assert!(allocations(&snapshot).status.contains("created after"));
        assert!(allocations(&snapshot).bases.is_empty());
        snapshot.items[0].identity_uid = Some("old-generation".into());
        enrich(&mut snapshot, &list(object.clone()))?;
        assert!(allocations(&snapshot).status.contains("UID changed"));
        let mut no_time = object;
        no_time["metadata"]["creationTimestamp"] = Value::Null;
        snapshot.items[0].identity_uid = None;
        enrich(&mut snapshot, &list(no_time))?;
        assert!(allocations(&snapshot).bases.is_empty());
        Ok(())
    }
    #[test]
    fn namespace_container_identity_and_resource_bounds_are_enforced_before_projection()
    -> Result<()> {
        let mut snapshot = metrics::demo(MetricsScope::Pods, "demo")?;
        let mut other = pod(json!({"cpu":"250m"}), json!({}));
        other["metadata"]["namespace"] = "other".into();
        assert!(enrich(&mut snapshot, &list(other)).is_err());
        assert!(snapshot.items[0].allocations.is_none());
        let object = pod(json!({}), json!({}));
        assert!(
            enrich(
                &mut snapshot,
                &json!({"items":[object.clone(), object.clone()]})
            )
            .is_err()
        );
        assert!(
            enrich(
                &mut snapshot,
                &json!({"items":vec![object.clone();MAX_OBJECTS+1]})
            )
            .is_err()
        );
        let mut oversized = object.clone();
        oversized["metadata"]["annotations"] = json!({"payload":"x".repeat(MAX_BYTES)});
        assert!(enrich(&mut snapshot, &list(oversized)).is_err());
        let mut duplicate_container = object.clone();
        duplicate_container["spec"]["containers"] = json!([{"name":"app"},{"name":"app"}]);
        enrich(&mut snapshot, &list(duplicate_container))?;
        assert!(allocations(&snapshot).bases.is_empty());
        assert!(
            allocations(&snapshot)
                .status
                .contains("duplicate container")
        );
        let mut absent = object;
        absent["metadata"]["name"] = "other-pod".into();
        enrich(&mut snapshot, &json!({"items":[absent],"truncated":true}))?;
        assert!(allocations(&snapshot).status.contains("resource missing"));
        assert!(
            snapshot
                .allocation_status
                .as_deref()
                .is_some_and(|status| status.contains("truncated"))
        );
        Ok(())
    }
    #[test]
    fn node_capacity_and_allocatable_are_separate_snapshot_bases() -> Result<()> {
        let mut snapshot = metrics::demo(MetricsScope::Nodes, "*")?;
        let node = json!({"kind":"Node","metadata":{"name":"demo-node","uid":"demo-Node-demo-node","creationTimestamp":"2026-10-10T07:00:00Z"},"status":{"allocatable":{"cpu":"3500m","memory":"7Gi"},"capacity":{"cpu":"4","memory":"8Gi"}}});
        enrich(&mut snapshot, &list(node))?;
        assert_eq!(allocations(&snapshot).bases[0].basis, "allocatable");
        assert_eq!(allocations(&snapshot).bases[1].basis, "capacity");
        assert_eq!(
            allocations(&snapshot).bases[1]
                .cpu
                .usage_percent
                .as_ref()
                .map(|percent| percent.value.as_str()),
            Some("18.75")
        );
        assert_eq!(
            allocations(&snapshot).bases[1]
                .memory
                .usage_percent
                .as_ref()
                .map(|percent| percent.value.as_str()),
            Some("12.50")
        );
        assert!(
            allocations(&snapshot).bases[0]
                .cpu
                .usage_percent
                .as_ref()
                .is_some_and(|percent| percent.rounded)
        );
        Ok(())
    }
    #[test]
    fn default_machine_output_omits_optional_comparisons() -> Result<()> {
        let snapshot = metrics::demo(MetricsScope::Pods, "demo")?;
        let machine = serde_json::to_string(&snapshot).map_err(|_| "test encoding failed")?;
        assert!(!machine.contains("allocations") && !machine.contains("allocation_status"));
        Ok(())
    }
}
