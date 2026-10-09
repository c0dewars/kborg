use crate::Result;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Scale(i32),
    Restart,
    Delete,
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
            Action::Scale(n) if target.resource == "deployments" && (0..=20).contains(n) => Ok(()),
            Action::Restart if target.resource == "deployments" => Ok(()),
            Action::Delete
                if ["pods", "deployments", "services", "jobs"]
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
        for action in [Action::Restart, Action::Scale(2), Action::Delete] {
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
    fn restricts_scope_and_scale() {
        let p = Policy { writable: true };
        let mut t = target();
        assert!(p.check(&t, &Action::Scale(21)).is_err());
        t.namespace = "kube-system".into();
        assert!(p.check(&t, &Action::Restart).is_err());
    }
}
