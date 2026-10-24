//! Kubernetes error mapping without echoing raw server or transport text.
use super::*;

pub(super) fn api_error(error: kube::Error) -> String {
    match error {
        kube::Error::Api(response) => format!(
            "Kubernetes API {} ({}); server message suppressed",
            response.code,
            safety::safe_text(&response.reason, 80)
        ),
        other => format!(
            "{}. Data may be stale and any write outcome is unknown.",
            transport_hint(&other)
        ),
    }
}
/// Names the likely cause of a transport failure without echoing raw error
/// text, which can carry endpoints or credential-adjacent detail.
pub(super) fn transport_hint(error: &(dyn std::error::Error + 'static)) -> &'static str {
    let mut chain = String::new();
    let mut current = Some(error);
    while let Some(error) = current {
        chain.push_str(&error.to_string().to_ascii_lowercase());
        chain.push(' ');
        current = error.source();
    }
    if chain.contains("connection refused") {
        "Cannot reach the Kubernetes API server (connection refused): is the cluster running? :ctx selects another context"
    } else if chain.contains("dns")
        || chain.contains("failed to lookup")
        || chain.contains("nodename nor servname")
    {
        "Cannot resolve the API server host name: check the network, VPN or kubeconfig server"
    } else if chain.contains("certificate")
        || chain.contains("unknownissuer")
        || chain.contains("tls")
    {
        "TLS verification of the API server failed: the cluster CA in kubeconfig does not match (verification is never skipped)"
    } else if chain.contains("timed out") || chain.contains("deadline") || chain.contains("elapsed")
    {
        "The API server did not answer in time: the cluster may be overloaded or unreachable"
    } else if chain.contains("unauthorized")
        || chain.contains("credential")
        || chain.contains("exec")
    {
        "Authentication with the API server failed: refresh your credentials (for example your cloud login)"
    } else {
        "Kubernetes transport or response failure"
    }
}
