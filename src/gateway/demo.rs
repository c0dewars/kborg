//! Offline demo objects, including KBORG_DEMO_SCALE synthetic Pods.
use super::*;

pub(super) fn demo_objects() -> Vec<Value> {
    let object = |kind: &str, name: &str, status: Value, spec: Value| {
        let resource = kinds().into_iter().find(|resource| resource.kind == kind);
        let version = resource
            .as_ref()
            .map(|resource| resource.ar().api_version)
            .unwrap_or_else(|| "v1".into());
        let mut value = json!({"apiVersion":version,"kind":kind,"metadata":{"name":name,"uid":format!("demo-{kind}-{name}"),"resourceVersion":"1","creationTimestamp":"2026-10-10T07:00:00Z"},"spec":spec,"status":status});
        if resource.is_some_and(|resource| resource.namespaced) {
            value["metadata"]["namespace"] = json!("demo");
        }
        value
    };
    let mut objects = vec![
        object(
            "Pod",
            "api-7d4f9",
            json!({"phase":"Running","containerStatuses":[{"name":"app","ready":true,"restartCount":0,"state":{"running":{}}}]}),
            json!({"nodeName":"demo-node","containers":[{"name":"app","image":"example/demo:v1","resources":{"requests":{"cpu":"250m","memory":"128Mi"},"limits":{"cpu":"1","memory":"512Mi"}}}]}),
        ),
        object(
            "Pod",
            "worker-6a1bc",
            json!({"phase":"Running","containerStatuses":[{"name":"app","ready":false,"restartCount":7,"state":{"waiting":{"reason":"CrashLoopBackOff"}}}]}),
            json!({"nodeName":"demo-node","containers":[{"name":"app","image":"example/demo:v1","resources":{"requests":{"cpu":"250m","memory":"128Mi"},"limits":{"cpu":"1","memory":"512Mi"}}}]}),
        ),
        object(
            "Deployment",
            "api",
            json!({"readyReplicas":2,"availableReplicas":2,"updatedReplicas":2}),
            json!({"replicas":2,"template":{"spec":{"containers":[{"name":"app","image":"example/demo:v1"}]}}}),
        ),
        object(
            "Service",
            "api",
            json!({}),
            json!({"type":"ClusterIP","clusterIP":"10.96.10.4","ports":[{"port":8080}]}),
        ),
        object("Namespace", "demo", json!({"phase":"Active"}), json!({})),
        object(
            "StatefulSet",
            "redis",
            json!({"readyReplicas":1}),
            json!({"replicas":1,"serviceName":"redis","updateStrategy":{"type":"RollingUpdate"},"template":{"spec":{"containers":[{"name":"redis","image":"redis:7","resources":{"requests":{"cpu":"100m","memory":"64Mi"},"limits":{"cpu":"500m","memory":"256Mi"}}}]}}}),
        ),
        object(
            "DaemonSet",
            "collector",
            json!({"numberReady":1,"desiredNumberScheduled":1}),
            json!({"updateStrategy":{"type":"RollingUpdate"},"template":{"spec":{"containers":[{"name":"collector","image":"example/collector:v1","resources":{"requests":{"cpu":"25m","memory":"32Mi"},"limits":{"cpu":"100m","memory":"128Mi"}}}]}}}),
        ),
        object(
            "ReplicaSet",
            "api-7d4f9",
            json!({"readyReplicas":2}),
            json!({"replicas":2}),
        ),
        object(
            "Job",
            "schema-migrate",
            json!({"succeeded":1,"conditions":[{"type":"Complete","status":"True"}]}),
            json!({"completions":1}),
        ),
        object(
            "CronJob",
            "nightly-backup",
            json!({"active":[]}),
            json!({"schedule":"0 2 * * *","suspend":false,"jobTemplate":{"spec":{"template":{"spec":{"restartPolicy":"Never","containers":[{"name":"backup","image":"example/backup:v1","resources":{"requests":{"cpu":"100m","memory":"64Mi"},"limits":{"cpu":"500m","memory":"256Mi"}}}]}}}}}),
        ),
        object(
            "Ingress",
            "api",
            json!({}),
            json!({"rules":[{"host":"api.demo.local"}]}),
        ),
        object("ConfigMap", "api-config", json!({}), json!({})),
        object(
            "PersistentVolumeClaim",
            "redis-data",
            json!({"phase":"Bound"}),
            json!({"storageClassName":"standard","resources":{"requests":{"storage":"1Gi"}}}),
        ),
        object(
            "Node",
            "demo-node",
            json!({"conditions":[{"type":"Ready","status":"True"}],"capacity":{"cpu":"4","memory":"8Gi"},"allocatable":{"cpu":"3500m","memory":"7Gi"}}),
            json!({"unschedulable":false}),
        ),
        object("Role", "viewer", json!({}), json!({})),
        object("RoleBinding", "viewer", json!({}), json!({})),
        object("StorageClass", "standard", json!({}), json!({})),
    ];
    objects.extend([
        object("ServiceAccount", "reader", json!({}), json!({})),
        object("PersistentVolume", "demo-volume", json!({"phase":"Bound"}), json!({"capacity":{"storage":"1Gi"},"storageClassName":"standard","accessModes":["ReadWriteOnce"],"claimRef":{"namespace":"demo","name":"redis-data"}})),
        object("Endpoints", "api", json!({}), json!({})),
        object("ReplicationController", "legacy-worker", json!({"readyReplicas":1}), json!({"replicas":1,"selector":{"app":"legacy-worker"},"template":{"metadata":{"labels":{"app":"legacy-worker"}},"spec":{"containers":[{"name":"worker","image":"example/worker:v1"}]}}})),
        object("ResourceQuota", "team-budget", json!({"hard":{"requests.cpu":"4","requests.memory":"8Gi"},"used":{"requests.cpu":"500m","requests.memory":"256Mi"}}), json!({"hard":{"requests.cpu":"4","requests.memory":"8Gi"}})),
        object("LimitRange", "container-defaults", json!({}), json!({"limits":[{"type":"Container","default":{"cpu":"500m","memory":"256Mi"},"defaultRequest":{"cpu":"100m","memory":"64Mi"}}]})),
        object("EndpointSlice", "api-demo", json!({}), json!({})),
        object("HorizontalPodAutoscaler", "api", json!({"currentReplicas":2,"desiredReplicas":2}), json!({"scaleTargetRef":{"apiVersion":"apps/v1","kind":"Deployment","name":"api"},"minReplicas":2,"maxReplicas":5,"metrics":[{"type":"Resource","resource":{"name":"cpu","target":{"type":"Utilization","averageUtilization":75}}}]})),
        object("PodDisruptionBudget", "api", json!({"disruptionsAllowed":1,"currentHealthy":2,"desiredHealthy":1}), json!({"minAvailable":1,"selector":{"matchLabels":{"app":"api"}}})),
        object("NetworkPolicy", "api-ingress", json!({}), json!({"podSelector":{"matchLabels":{"app":"api"}},"policyTypes":["Ingress"],"ingress":[{"ports":[{"protocol":"TCP","port":8080}]}]})),
        object("CustomResourceDefinition", "widgets.demo.local", json!({"conditions":[{"type":"Established","status":"True"}]}), json!({"group":"demo.local","scope":"Namespaced","names":{"plural":"widgets","singular":"widget","kind":"Widget"},"versions":[{"name":"v1","served":true,"storage":true,"schema":{"openAPIV3Schema":{"type":"object"}}}]})),
        object("ClusterRole", "demo-reader", json!({}), json!({})),
        object("ClusterRoleBinding", "demo-reader", json!({}), json!({})),
        object("PriorityClass", "batch-priority", json!({}), json!({})),
        object("RuntimeClass", "standard", json!({}), json!({})),
        object("CertificateSigningRequest", "demo-client", json!({"conditions":[]}), json!({"signerName":"kubernetes.io/kube-apiserver-client","usages":["client auth"],"username":"demo-user","request":"synthetic-example-no-private-key"})),
        object("VolumeAttachment", "demo-attachment", json!({"attached":true}), json!({"attacher":"demo.local/storage","nodeName":"demo-node","source":{"persistentVolumeName":"demo-volume"}})),
        object("APIService", "v1beta1.metrics.k8s.io", json!({"conditions":[{"type":"Available","status":"True"}]}), json!({"group":"metrics.k8s.io","version":"v1beta1","groupPriorityMinimum":100,"versionPriority":100,"service":{"name":"metrics-server","namespace":"kube-system"}})),
    ]);
    let mut event = object("Event", "worker-backoff", json!({}), json!({}));
    event["type"] = json!("Warning");
    event["reason"] = json!("BackOff");
    event["message"] = json!("Back-off restarting failed container app in worker-6a1bc");
    event["count"] = json!(7);
    event["involvedObject"] = json!({"kind":"Pod","namespace":"demo","name":"worker-6a1bc"});
    objects.push(event);
    for object in &mut objects {
        match object["kind"].as_str().unwrap_or("") {
            "Pod" if object["metadata"]["name"] == "api-7d4f9" => {
                object["metadata"]["labels"] = json!({"app":"api"});
                object["metadata"]["ownerReferences"] = json!([{"apiVersion":"apps/v1","kind":"ReplicaSet","name":"api-7d4f9","uid":"demo-ReplicaSet-api-7d4f9","controller":true}]);
            }
            "Pod" => {
                object["metadata"]["labels"] = json!({"app":"worker"});
            }
            "ReplicaSet" => {
                object["metadata"]["labels"] = json!({"app":"api"});
                object["metadata"]["ownerReferences"] = json!([{"apiVersion":"apps/v1","kind":"Deployment","name":"api","uid":"demo-Deployment-api","controller":true}]);
            }
            "Deployment" => {
                object["metadata"]["labels"] = json!({"app":"api"});
                object["spec"]["selector"] = json!({"matchLabels":{"app":"api"}});
                object["spec"]["template"]["metadata"]["labels"] = json!({"app":"api"});
            }
            "Service" => {
                object["spec"]["selector"] = json!({"app":"api"});
            }
            "Role" | "ClusterRole" => {
                object["rules"] = json!([{"apiGroups":[""],"resources":["pods","services"],"verbs":["get","list","watch"]}]);
            }
            "RoleBinding" => {
                object["subjects"] = json!([{"kind":"ServiceAccount","name":"reader","namespace":"demo"},{"kind":"User","apiGroup":"rbac.authorization.k8s.io","name":"demo-user"}]);
                object["roleRef"] =
                    json!({"apiGroup":"rbac.authorization.k8s.io","kind":"Role","name":"viewer"});
            }
            "ClusterRoleBinding" => {
                object["subjects"] = json!([{"kind":"Group","apiGroup":"rbac.authorization.k8s.io","name":"demo-viewers"}]);
                object["roleRef"] = json!({"apiGroup":"rbac.authorization.k8s.io","kind":"ClusterRole","name":"demo-reader"});
            }
            "ServiceAccount" => {
                object["automountServiceAccountToken"] = json!(false);
            }
            "Endpoints" => {
                object["subsets"] = json!([{"addresses":[{"ip":"10.0.0.10","targetRef":{"kind":"Pod","namespace":"demo","name":"api-7d4f9"}}],"ports":[{"port":8080,"protocol":"TCP"}]}]);
            }
            "EndpointSlice" => {
                object["metadata"]["labels"] = json!({"kubernetes.io/service-name":"api"});
                object["addressType"] = json!("IPv4");
                object["endpoints"] = json!([{"addresses":["10.0.0.10"],"conditions":{"ready":true},"targetRef":{"kind":"Pod","namespace":"demo","name":"api-7d4f9"}}]);
                object["ports"] = json!([{"port":8080,"protocol":"TCP"}]);
            }
            "PriorityClass" => {
                object["value"] = json!(1000);
                object["globalDefault"] = json!(false);
            }
            "RuntimeClass" => {
                object["handler"] = json!("runc");
            }
            _ => {}
        }
        if object["kind"] == "StorageClass" {
            object["provisioner"] = json!("demo.local/storage");
        }
        if object["kind"] == "ConfigMap" {
            object["data"] = json!({"endpoint":"synthetic-example"});
        }
    }
    objects.extend(demo_scale_pods(&object));
    objects
}

/// `KBORG_DEMO_SCALE=N` adds N synthetic Running Pods (at most 20,000) to demo
/// data, so scale and performance checks run without a cluster.
pub(super) fn demo_scale_pods(object: &dyn Fn(&str, &str, Value, Value) -> Value) -> Vec<Value> {
    let count = std::env::var("KBORG_DEMO_SCALE")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0)
        .min(20_000);
    (0..count)
        .map(|index| {
            object(
                "Pod",
                &format!("scale-{index:05}"),
                json!({"phase":"Running","conditions":[{"type":"Ready","status":"True"}],"containerStatuses":[{"name":"app","ready":true,"restartCount":index % 4,"state":{"running":{}}}]}),
                json!({"nodeName":"demo-node","containers":[{"name":"app","image":"example/demo:v1"}]}),
            )
        })
        .collect()
}
