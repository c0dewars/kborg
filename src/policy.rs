use crate::Result;
use std::time::{Duration, Instant};

#[derive(Clone, PartialEq, Eq)]
pub enum Action {
    Scale(i32),
    SetImage {
        container: String,
        image: String,
    },
    SetResources {
        container: String,
        resources: crate::edit::ResourceEdit,
    },
    SetContainer {
        container: String,
        edit: crate::edit::ContainerEdit,
    },
    Restart,
    Delete,
    /// Pod-only delete with gracePeriodSeconds=0 (k9s Ctrl-K).
    Kill,
    /// CronJob: create a Job from its template now (k9s `t`).
    Trigger,
    /// CronJob: set `spec.suspend` (true suspends, false resumes).
    Suspend(bool),
    /// Deployment/StatefulSet/DaemonSet: restore a revision's Pod template;
    /// `None` means the previous revision.
    Rollback(Option<u64>),
}

impl Action {
    pub fn operation_name(&self) -> &'static str {
        match self {
            Self::Scale(_) => "scale",
            Self::Restart => "restart",
            Self::Delete => "delete",
            Self::Kill => "kill",
            Self::SetImage { .. } => "set-image",
            Self::SetResources { .. } => "set-resources",
            Self::SetContainer { .. } => "edit-container",
            Self::Trigger => "trigger",
            Self::Suspend(true) => "suspend",
            Self::Suspend(false) => "resume",
            Self::Rollback(_) => "rollback",
        }
    }
}
impl std::fmt::Debug for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.operation_name())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub session: String,
    pub resource: String,
    pub namespace: String,
    pub name: String,
    pub uid: String,
    pub version: String,
}

impl Target {
    /// What a typed confirmation must equal: `namespace/name`, or the name
    /// alone for a cluster-scoped object.
    pub fn confirmation(&self) -> String {
        if self.namespace.is_empty() {
            self.name.clone()
        } else {
            format!("{}/{}", self.namespace, self.name)
        }
    }
}

pub struct Approval {
    target: Target,
    action: Action,
    issued: Instant,
    consumed: bool,
}

/// Most replicas a reviewed scale may ask for.
pub const MAX_REPLICAS: i32 = 10_000;

pub fn system_namespace(namespace: &str) -> bool {
    ["kube-system", "kube-public", "kube-node-lease"].contains(&namespace)
}

#[derive(Clone)]
pub struct Policy {
    pub writable: bool,
    /// Changes in kube-system, kube-public and kube-node-lease
    /// (operator config `allow_system_namespaces`).
    pub system_namespaces: bool,
    /// Changes to cluster-scoped objects such as Nodes, Namespaces and
    /// ClusterRoles (operator config `allow_cluster_scoped`).
    pub cluster_scoped: bool,
}
impl Policy {
    /// The restrictive policy: no system namespaces, no cluster-scoped objects.
    pub fn strict(writable: bool) -> Self {
        Self {
            writable,
            system_namespaces: false,
            cluster_scoped: false,
        }
    }
    /// Whether the target's scope is open to changes at all.
    pub fn scope(&self, namespace: &str) -> Result<()> {
        if namespace.is_empty() {
            if !self.cluster_scoped {
                return Err(
                    "cluster-scoped changes are off (operator config allow_cluster_scoped: false)"
                        .into(),
                );
            }
        } else if system_namespace(namespace) && !self.system_namespaces {
            return Err(
                "system-namespace changes are off (operator config allow_system_namespaces: false)"
                    .into(),
            );
        }
        Ok(())
    }
}

impl Policy {
    pub fn check(&self, target: &Target, action: &Action) -> Result<()> {
        if !self.writable {
            return Err(
                "READ ONLY: changes are off in this session (--read-only or operator config)"
                    .into(),
            );
        }
        self.scope(&target.namespace)?;
        if target.uid.is_empty() || target.version.is_empty() {
            return Err("target has no UID/resourceVersion".into());
        }
        match action {
            Action::Scale(n)
                if ["deployments", "statefulsets", "replicasets"]
                    .contains(&target.resource.as_str())
                    && (0..=MAX_REPLICAS).contains(n) =>
            {
                Ok(())
            }
            Action::SetImage { container, image }
                if crate::edit::supports_resource(&target.resource) =>
            {
                crate::edit::validate(container, image)
            }
            Action::SetResources {
                container,
                resources,
            } if crate::edit::supports_resource(&target.resource) => resources.validate(container),
            Action::SetContainer { container, edit }
                if crate::edit::supports_resource(&target.resource) =>
            {
                edit.validate(container)
            }
            Action::Restart
                if ["deployments", "statefulsets", "daemonsets"]
                    .contains(&target.resource.as_str()) =>
            {
                Ok(())
            }
            // Any kind can be deleted after its review, as in kubectl and k9s.
            Action::Delete => Ok(()),
            Action::Kill if target.resource == "pods" => Ok(()),
            Action::Trigger | Action::Suspend(_) if target.resource == "cronjobs" => Ok(()),
            Action::Rollback(revision)
                if ["deployments", "statefulsets", "daemonsets"]
                    .contains(&target.resource.as_str())
                    && revision.is_none_or(|r| r > 0) =>
            {
                Ok(())
            }
            _ => Err(format!(
                "this action is not available for {} (scale range: 0..{MAX_REPLICAS})",
                target.resource
            )),
        }
    }
}

impl Approval {
    pub fn confirm(target: Target, action: Action, typed: &str) -> Result<Self> {
        if typed != target.confirmation() {
            return Err("confirmation must exactly match namespace/name".into());
        }
        Ok(Self {
            target,
            action,
            issued: Instant::now(),
            consumed: false,
        })
    }
    pub fn consume(&mut self, target: &Target, action: &Action) -> Result<()> {
        if self.consumed {
            return Err("approval already consumed".into());
        }
        self.consumed = true;
        if self.issued.elapsed() > Duration::from_secs(60) {
            return Err("approval expired; review again".into());
        }
        if &self.target != target || &self.action != action {
            return Err("target or action changed; review again".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn workload_operations_respect_family_write_and_namespace_boundaries() {
        let mut target = target();
        let mut policy = Policy::strict(true);
        for resource in ["deployments", "replicasets", "statefulsets"] {
            target.resource = resource.into();
            assert!(policy.check(&target, &Action::Scale(0)).is_ok());
        }
        // A ReplicaSet scales, but restart and image changes belong to its owner.
        target.resource = "replicasets".into();
        assert!(policy.check(&target, &Action::Restart).is_err());
        target.resource = "statefulsets".into();
        assert!(
            policy
                .check(&target, &Action::Scale(MAX_REPLICAS + 1))
                .is_err()
        );
        for resource in ["deployments", "statefulsets", "daemonsets"] {
            target.resource = resource.into();
            assert!(policy.check(&target, &Action::Restart).is_ok());
            assert!(policy.check(&target, &Action::Delete).is_ok());
        }
        target.resource = "daemonsets".into();
        assert!(policy.check(&target, &Action::Scale(2)).is_err());
        target.resource = "cronjobs".into();
        assert!(policy.check(&target, &Action::Delete).is_ok());
        assert!(policy.check(&target, &Action::Restart).is_err());
        target.namespace = "kube-system".into();
        assert!(policy.check(&target, &Action::Delete).is_err());
        target.namespace = "test".into();
        policy.writable = false;
        assert!(policy.check(&target, &Action::Delete).is_err());
    }
    #[test]
    fn atomic_container_edit_is_scoped_and_binds_every_reviewed_field() -> Result<()> {
        let action = Action::SetContainer {
            container: "app".into(),
            edit: crate::edit::ContainerEdit {
                image: "example/app:2".into(),
                resources: None,
            },
        };
        let policy = Policy::strict(true);
        for resource in ["deployments", "statefulsets", "daemonsets", "cronjobs"] {
            let mut target = target();
            target.resource = resource.into();
            policy.check(&target, &action)?;
            let mut approval = Approval::confirm(target.clone(), action.clone(), "test/app")?;
            let mut changed = action.clone();
            if let Action::SetContainer { edit, .. } = &mut changed {
                edit.resources = Some(crate::edit::ResourceEdit {
                    cpu_request: "100m".into(),
                    memory_request: "64Mi".into(),
                    cpu_limit: "200m".into(),
                    memory_limit: "128Mi".into(),
                });
            }
            assert!(approval.consume(&target, &changed).is_err());
        }
        for resource in ["pods", "jobs", "replicasets", "customworkloads"] {
            let mut target = target();
            target.resource = resource.into();
            assert!(policy.check(&target, &action).is_err());
        }
        Ok(())
    }
    fn target() -> Target {
        Target {
            session: "a".into(),
            resource: "deployments".into(),
            namespace: "test".into(),
            name: "app".into(),
            uid: "u".into(),
            version: "1".into(),
        }
    }
    #[test]
    fn approval_expires_and_success_cannot_replay() {
        let mut a = Approval::confirm(target(), Action::Restart, "test/app")
            .unwrap_or_else(|e| panic!("{e}"));
        a.issued = Instant::now() - Duration::from_secs(61);
        assert!(a.consume(&target(), &Action::Restart).is_err());
        let mut b = Approval::confirm(target(), Action::Restart, "test/app")
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(b.consume(&target(), &Action::Restart).is_ok());
        assert!(b.consume(&target(), &Action::Restart).is_err());
    }
    #[test]
    fn read_only_rejects_all_writes() {
        for action in [
            Action::Restart,
            Action::Scale(2),
            Action::Delete,
            Action::SetImage {
                container: "api".into(),
                image: "private/image:tag".into(),
            },
        ] {
            assert!(Policy::strict(false).check(&target(), &action).is_err());
        }
    }
    #[test]
    fn approval_is_single_use_and_target_bound() {
        let mut a = Approval::confirm(target(), Action::Restart, "test/app")
            .unwrap_or_else(|e| panic!("{e}"));
        let mut other = target();
        other.session = "b".into();
        assert!(a.consume(&other, &Action::Restart).is_err());
        assert!(a.consume(&target(), &Action::Restart).is_err());
    }
    #[test]
    fn image_approval_binds_values_and_debug_omits_values() {
        let action = Action::SetImage {
            container: "api".into(),
            image: "private-canary/image:v1".into(),
        };
        assert_eq!(format!("{action:?}"), "set-image");
        let mut approval =
            Approval::confirm(target(), action, "test/app").unwrap_or_else(|e| panic!("{e}"));
        assert!(
            approval
                .consume(
                    &target(),
                    &Action::SetImage {
                        container: "api".into(),
                        image: "private-canary/image:v2".into()
                    }
                )
                .is_err()
        );
    }
    #[test]
    fn open_policy_reaches_system_namespaces_and_cluster_objects() {
        let open = Policy {
            writable: true,
            system_namespaces: true,
            cluster_scoped: true,
        };
        let strict = Policy::strict(true);
        let mut t = target();
        // Delete is for every kind; workload actions are not.
        t.resource = "configmaps".into();
        assert!(strict.check(&t, &Action::Delete).is_ok());
        assert!(strict.check(&t, &Action::Restart).is_err());
        t.namespace = "kube-system".into();
        assert!(open.check(&t, &Action::Delete).is_ok());
        assert!(
            strict
                .check(&t, &Action::Delete)
                .is_err_and(|error| error.contains("allow_system_namespaces"))
        );
        // A cluster-scoped object has no namespace and is confirmed by name.
        t.namespace.clear();
        t.resource = "nodes".into();
        assert!(open.check(&t, &Action::Delete).is_ok());
        assert!(
            strict
                .check(&t, &Action::Delete)
                .is_err_and(|error| error.contains("allow_cluster_scoped"))
        );
        assert_eq!(t.confirmation(), "app");
        assert!(Approval::confirm(t.clone(), Action::Delete, "app").is_ok());
        assert!(Approval::confirm(t, Action::Delete, "/app").is_err());
        // Read-only wins over everything.
        let mut read_only = open;
        read_only.writable = false;
        assert!(read_only.check(&target(), &Action::Delete).is_err());
    }
    #[test]
    fn restricts_scope_and_scale() {
        let p = Policy::strict(true);
        let mut t = target();
        assert!(p.check(&t, &Action::Scale(MAX_REPLICAS + 1)).is_err());
        t.namespace = "kube-system".into();
        assert!(p.check(&t, &Action::Restart).is_err());
    }
}
