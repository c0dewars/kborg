//! Kubernetes error mapping for gateway calls; classification and wording
//! live in `crate::errors` so every failure names its cause and next step.
use super::*;

impl Gateway {
    pub(crate) fn scope(&self) -> crate::errors::Scope<'_> {
        crate::errors::Scope {
            context: &self.context,
            server: &self.server,
            namespace: &self.namespace,
        }
    }
    /// Discovery errors are local strings; give each one a next step.
    pub(super) fn discovery_error(&self, error: String) -> String {
        if error.contains(crate::errors::NEXT) {
            return error;
        }
        let next = if error.contains("transport") || error.contains("deadline") {
            format!(
                "check VPN/proxy and network access to {}, then run :discover again",
                crate::errors::host(&self.server)
            )
        } else {
            "run :discover again; if it repeats, an aggregated API may be broken (`kubectl get apiservices`)".into()
        };
        format!(
            "API discovery failed: {error}. {} {next}.",
            crate::errors::NEXT
        )
    }
    /// Actionable text for a failed Kubernetes API call made by this gateway.
    pub(super) fn api_error(&self, error: kube::Error) -> String {
        crate::errors::explain("Kubernetes request", &error, &self.scope())
    }
}
