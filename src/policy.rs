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
}

impl Action {
    pub fn operation_name(&self) -> &'static str {
        match self {
            Self::Scale(_) => "scale",
            Self::Restart => "restart",
            Self::Delete => "delete",
            Self::SetImage { .. } => "set-image",
            Self::SetResources { .. } => "set-resources",
            Self::SetContainer { .. } => "edit-container",
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

pub struct Approval {
    target: Target,
    action: Action,
    issued: Instant,
    consumed: bool,
}

#[derive(Clone)]
pub struct Policy {
    pub writable: bool,
}

impl Policy {
    pub fn check(&self, target: &Target, action: &Action) -> Result<()> {
        if !self.writable {
            return Err("READ ONLY: mutations are disabled; opt in with --write".into());
        }
        if target.namespace.is_empty()
            || target.namespace == "kube-system"
            || target.namespace == "kube-public"
            || target.namespace == "kube-node-lease"
        {
            return Err("cluster-scoped and system-namespace mutations are disabled".into());
        }
        if target.uid.is_empty() || target.version.is_empty() {
            return Err("target has no UID/resourceVersion".into());
        }
        match action {
            Action::Scale(n)
                if ["deployments", "statefulsets"].contains(&target.resource.as_str())
                    && (0..=20).contains(n) =>
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
            Action::Delete
                if [
                    "pods",
                    "deployments",
                    "statefulsets",
                    "daemonsets",
                    "cronjobs",
                    "services",
                    "jobs",
                ]
                .contains(&target.resource.as_str()) =>
            {
                Ok(())
            }
            _ => Err("operation family is disabled by policy (scale range: 0..20)".into()),
        }
    }
}

impl Approval {
    pub fn confirm(target: Target, action: Action, typed: &str) -> Result<Self> {
        if typed != format!("{}/{}", target.namespace, target.name) {
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
        let mut policy = Policy { writable: true };
        target.resource = "statefulsets".into();
        assert!(policy.check(&target, &Action::Scale(0)).is_ok());
        assert!(policy.check(&target, &Action::Scale(21)).is_err());
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
        let policy = Policy { writable: true };
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
            assert!(
                Policy { writable: false }
                    .check(&target(), &action)
                    .is_err()
            );
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
    fn restricts_scope_and_scale() {
        let p = Policy { writable: true };
        let mut t = target();
        assert!(p.check(&t, &Action::Scale(21)).is_err());
        t.namespace = "kube-system".into();
        assert!(p.check(&t, &Action::Restart).is_err());
    }
}
