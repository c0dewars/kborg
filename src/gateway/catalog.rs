//! Built-in resource kinds, their aliases and API resource mapping.
use super::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Kind {
    pub plural: String,
    pub kind: String,
    pub group: String,
    pub version: String,
    pub namespaced: bool,
    #[serde(default)]
    pub short_names: Vec<String>,
}
impl Kind {
    pub(super) fn ar(&self) -> ApiResource {
        ApiResource {
            group: self.group.clone(),
            version: self.version.clone(),
            api_version: if self.group.is_empty() {
                self.version.clone()
            } else {
                format!("{}/{}", self.group, self.version)
            },
            kind: self.kind.clone(),
            plural: self.plural.clone(),
        }
    }
    pub fn key(&self) -> String {
        if self.group.is_empty() {
            self.plural.clone()
        } else {
            format!("{}.{}", self.plural, self.group)
        }
    }
}
pub(super) fn builtin_aliases(kind: &str) -> &'static [&'static str] {
    match kind {
        "Pod" => &["po"],
        "Deployment" => &["deploy", "dp"],
        "Service" => &["svc"],
        "Job" => &["jo"],
        "Role" => &["ro"],
        "RoleBinding" => &["rb"],
        "ClusterRole" => &["cr"],
        "ClusterRoleBinding" => &["crb"],
        "Event" => &["ev"],
        "StatefulSet" => &["sts"],
        "DaemonSet" => &["ds"],
        "ReplicaSet" => &["rs"],
        "CronJob" => &["cj"],
        "Ingress" => &["ing"],
        "ConfigMap" => &["cm"],
        "PersistentVolumeClaim" => &["pvc"],
        "Node" => &["no"],
        "Namespace" => &["ns"],
        "StorageClass" => &["sc"],
        "ServiceAccount" => &["sa"],
        "PersistentVolume" => &["pv"],
        "Endpoints" => &["ep"],
        "ReplicationController" => &["rc"],
        "HorizontalPodAutoscaler" => &["hpa"],
        "PodDisruptionBudget" => &["pdb"],
        "NetworkPolicy" => &["netpol", "np"],
        "ResourceQuota" => &["quota"],
        "LimitRange" => &["limits"],
        "CustomResourceDefinition" => &["crd", "crds"],
        "PriorityClass" => &["pc"],
        "CertificateSigningRequest" => &["csr"],
        "VolumeAttachment" => &["va"],
        _ => &[],
    }
}
pub fn kinds() -> Vec<Kind> {
    [
        ("pods", "Pod", "", "v1", true),
        ("deployments", "Deployment", "apps", "v1", true),
        ("services", "Service", "", "v1", true),
        ("events", "Event", "", "v1", true),
        ("statefulsets", "StatefulSet", "apps", "v1", true),
        ("daemonsets", "DaemonSet", "apps", "v1", true),
        ("replicasets", "ReplicaSet", "apps", "v1", true),
        ("jobs", "Job", "batch", "v1", true),
        ("cronjobs", "CronJob", "batch", "v1", true),
        ("ingresses", "Ingress", "networking.k8s.io", "v1", true),
        ("configmaps", "ConfigMap", "", "v1", true),
        (
            "persistentvolumeclaims",
            "PersistentVolumeClaim",
            "",
            "v1",
            true,
        ),
        ("nodes", "Node", "", "v1", false),
        ("namespaces", "Namespace", "", "v1", false),
        (
            "storageclasses",
            "StorageClass",
            "storage.k8s.io",
            "v1",
            false,
        ),
        ("roles", "Role", "rbac.authorization.k8s.io", "v1", true),
        ("serviceaccounts", "ServiceAccount", "", "v1", true),
        ("persistentvolumes", "PersistentVolume", "", "v1", false),
        ("endpoints", "Endpoints", "", "v1", true),
        (
            "replicationcontrollers",
            "ReplicationController",
            "",
            "v1",
            true,
        ),
        ("resourcequotas", "ResourceQuota", "", "v1", true),
        ("limitranges", "LimitRange", "", "v1", true),
        (
            "endpointslices",
            "EndpointSlice",
            "discovery.k8s.io",
            "v1",
            true,
        ),
        (
            "horizontalpodautoscalers",
            "HorizontalPodAutoscaler",
            "autoscaling",
            "v2",
            true,
        ),
        (
            "poddisruptionbudgets",
            "PodDisruptionBudget",
            "policy",
            "v1",
            true,
        ),
        (
            "networkpolicies",
            "NetworkPolicy",
            "networking.k8s.io",
            "v1",
            true,
        ),
        (
            "customresourcedefinitions",
            "CustomResourceDefinition",
            "apiextensions.k8s.io",
            "v1",
            false,
        ),
        (
            "clusterroles",
            "ClusterRole",
            "rbac.authorization.k8s.io",
            "v1",
            false,
        ),
        (
            "clusterrolebindings",
            "ClusterRoleBinding",
            "rbac.authorization.k8s.io",
            "v1",
            false,
        ),
        (
            "priorityclasses",
            "PriorityClass",
            "scheduling.k8s.io",
            "v1",
            false,
        ),
        ("runtimeclasses", "RuntimeClass", "node.k8s.io", "v1", false),
        (
            "certificatesigningrequests",
            "CertificateSigningRequest",
            "certificates.k8s.io",
            "v1",
            false,
        ),
        (
            "volumeattachments",
            "VolumeAttachment",
            "storage.k8s.io",
            "v1",
            false,
        ),
        (
            "apiservices",
            "APIService",
            "apiregistration.k8s.io",
            "v1",
            false,
        ),
        (
            "rolebindings",
            "RoleBinding",
            "rbac.authorization.k8s.io",
            "v1",
            true,
        ),
    ]
    .into_iter()
    .map(|(p, k, g, v, n)| Kind {
        plural: p.into(),
        kind: k.into(),
        group: g.into(),
        version: v.into(),
        namespaced: n,
        short_names: builtin_aliases(k).iter().map(|s| (*s).into()).collect(),
    })
    .collect()
}
