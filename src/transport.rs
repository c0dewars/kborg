//! Finite HTTP budgets; long logs and upgraded sessions have application deadlines.
use http::{Request, Response};
use http_body::{Body, Frame, SizeHint};
use http_body_util::Limited;
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tower::{Layer, Service};

type Error = Box<dyn std::error::Error + Send + Sync>;
const OBJECT_BYTES: usize = 8 * 1024 * 1024;
pub const LOG_STREAM_BYTES: usize = 64 * 1024 * 1024;
const HEADER_BUDGET: Duration = Duration::from_secs(15);
const OBJECT_IDLE: Duration = Duration::from_secs(10);

#[derive(Clone, Copy)]
pub struct BodyBudgetLayer {
    objects: usize,
    logs: usize,
}
impl Default for BodyBudgetLayer {
    fn default() -> Self {
        Self {
            objects: OBJECT_BYTES,
            logs: LOG_STREAM_BYTES,
        }
    }
}
#[derive(Clone)]
pub struct BodyBudget<S> {
    inner: S,
    budgets: BodyBudgetLayer,
}
impl<S> Layer<S> for BodyBudgetLayer {
    type Service = BodyBudget<S>;
    fn layer(&self, inner: S) -> Self::Service {
        BodyBudget {
            inner,
            budgets: *self,
        }
    }
}
impl<S, RequestBody, B> Service<Request<RequestBody>> for BodyBudget<S>
where
    S: Service<Request<RequestBody>, Response = Response<B>>,
    S::Future: Send + 'static,
    S::Error: Into<Error> + 'static,
    B: Body + 'static,
    B::Data: From<Vec<u8>>,
    B::Error: Into<Error>,
{
    type Response = Response<Limited<IdleBody<B>>>;
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }
    fn call(&mut self, request: Request<RequestBody>) -> Self::Future {
        let watch = request.method() == http::Method::GET
            && request
                .uri()
                .query()
                .is_some_and(|query| query.split('&').any(|entry| entry == "watch=true"));
        let stream = request.uri().path().contains("/pods/")
            && request.uri().path().ends_with("/log")
            && request
                .uri()
                .query()
                .is_some_and(|query| query.split('&').any(|entry| entry == "follow=true"));
        let limit = if stream {
            self.budgets.logs
        } else {
            self.budgets.objects
        };
        let idle = (!stream).then_some(OBJECT_IDLE);
        let response = self.inner.call(request);
        Box::pin(async move {
            match tokio::time::timeout(HEADER_BUDGET, response).await {
                Ok(Ok(mut response)) => {
                    // kube-client request_events decodes stream lines without
                    // checking HTTP status. Never let a blank/hostile error body
                    // masquerade as a successful empty watch or change its code.
                    if watch && !response.status().is_success() {
                        let bytes = format!(
                            "{{\"apiVersion\":\"v1\",\"kind\":\"Status\",\"status\":\"Failure\",\"reason\":\"WatchHttpError\",\"message\":\"watch HTTP response rejected; server body withheld\",\"code\":{}}}\n",
                            response.status().as_u16()
                        ).into_bytes();
                        let headers = response.headers_mut();
                        headers.remove(http::header::CONTENT_ENCODING);
                        headers.remove(http::header::TRANSFER_ENCODING);
                        headers.remove(http::header::CONTENT_LENGTH);
                        headers.insert(
                            http::header::CONTENT_TYPE,
                            http::HeaderValue::from_static("application/json"),
                        );
                        // map retains the original HTTP status and extensions,
                        // including those inspected by the outer auth gate.
                        Ok(response.map(|_body| Limited::new(IdleBody::<B>::owned(bytes), limit)))
                    } else {
                        Ok(response.map(|body| Limited::new(IdleBody::new(body, idle), limit)))
                    }
                }
                Ok(Err(error)) => Err(error.into()),
                Err(_) => Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "HTTP response headers exceeded 15-second budget",
                )
                .into()),
            }
        })
    }
}

/// Uses the maintained HTTP Body trait; no protocol or wire-format parsing.
pub struct IdleBody<B> {
    inner: Option<Pin<Box<B>>>,
    owned: Option<Vec<u8>>,
    budget: Option<Duration>,
    timer: Option<Pin<Box<tokio::time::Sleep>>>,
    ended: bool,
}
impl<B> IdleBody<B> {
    fn new(body: B, budget: Option<Duration>) -> Self {
        Self {
            inner: Some(Box::pin(body)),
            owned: None,
            budget,
            timer: budget.map(|budget| Box::pin(tokio::time::sleep(budget))),
            ended: false,
        }
    }
    fn owned(bytes: Vec<u8>) -> Self {
        Self {
            inner: None,
            owned: Some(bytes),
            budget: None,
            timer: None,
            ended: false,
        }
    }
}
impl<B> Body for IdleBody<B>
where
    B: Body,
    B::Data: From<Vec<u8>>,
    B::Error: Into<Error>,
{
    type Data = B::Data;
    type Error = Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        if this.ended {
            return Poll::Ready(None);
        }
        if let Some(bytes) = this.owned.take() {
            this.ended = true;
            return Poll::Ready(Some(Ok(Frame::data(bytes.into()))));
        }
        let Some(inner) = &mut this.inner else {
            this.ended = true;
            return Poll::Ready(None);
        };
        match inner.as_mut().poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let (Some(timer), Some(budget)) = (&mut this.timer, this.budget) {
                    timer.as_mut().reset(tokio::time::Instant::now() + budget);
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(error))) => {
                this.ended = true;
                Poll::Ready(Some(Err(error.into())))
            }
            Poll::Ready(None) => {
                this.ended = true;
                Poll::Ready(None)
            }
            Poll::Pending => {
                if this
                    .timer
                    .as_mut()
                    .is_some_and(|timer| timer.as_mut().poll(cx).is_ready())
                {
                    this.ended = true;
                    return Poll::Ready(Some(Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "HTTP object body idle budget exceeded",
                    )
                    .into())));
                }
                Poll::Pending
            }
        }
    }
    fn is_end_stream(&self) -> bool {
        self.ended
            || (self.owned.is_none() && self.inner.as_ref().is_none_or(|body| body.is_end_stream()))
    }
    fn size_hint(&self) -> SizeHint {
        if let Some(bytes) = &self.owned {
            SizeHint::with_exact(bytes.len() as u64)
        } else {
            self.inner
                .as_ref()
                .map_or_else(SizeHint::new, |body| body.size_hint())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http_body_util::{BodyExt, Full};
    use tower::ServiceExt;
    #[test]
    fn rejected_watch_uses_http_status_without_polling_remote_body() -> crate::Result<()> {
        struct Unread(std::sync::Arc<std::sync::atomic::AtomicBool>);
        impl Drop for Unread {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        impl Body for Unread {
            type Data = Bytes;
            type Error = std::convert::Infallible;
            fn poll_frame(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
            ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
                panic!("rejected watch response body must never be polled");
            }
        }
        crate::cli::runtime()?.block_on(async {
            let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let original = dropped.clone();
            let inner = tower::service_fn(move |_: Request<()>| {
                let original = original.clone();
                async move {
                    let mut response = Response::builder()
                        .status(403)
                        .header(http::header::CONTENT_TYPE, "text/html")
                        .header(http::header::CONTENT_ENCODING, "gzip")
                        .header(http::header::CONTENT_LENGTH, "999999999")
                        .header(http::header::TRANSFER_ENCODING, "chunked")
                        .body(Unread(original))
                        .map_err(|_| "invalid fixture response")?;
                    response.extensions_mut().insert(7u32);
                    Ok::<_, String>(response)
                }
            });
            let request = Request::builder()
                .uri("/api/v1/pods?watch=true&resourceVersion=1")
                .body(())
                .map_err(|_| "invalid fixture request")?;
            let response = BodyBudgetLayer::default()
                .layer(inner)
                .oneshot(request)
                .await
                .map_err(|_| "fixture transport failed")?;
            assert_eq!(response.status(), http::StatusCode::FORBIDDEN);
            assert_eq!(response.extensions().get::<u32>(), Some(&7));
            assert_eq!(
                response.headers()[http::header::CONTENT_TYPE],
                "application/json"
            );
            for header in [
                http::header::CONTENT_ENCODING,
                http::header::CONTENT_LENGTH,
                http::header::TRANSFER_ENCODING,
            ] {
                assert!(!response.headers().contains_key(header));
            }
            assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
            let bytes = response
                .into_body()
                .collect()
                .await
                .map_err(|_| "synthetic status read failed")?
                .to_bytes();
            assert!(bytes.len() < 512);
            let status: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|_| "synthetic status is invalid JSON")?;
            assert_eq!(status["kind"], "Status");
            assert_eq!(status["code"], 403);
            Ok(())
        })
    }

    #[test]
    fn ordinary_and_successful_watch_responses_are_unchanged() -> crate::Result<()> {
        crate::cli::runtime()?.block_on(async {
            for (method, uri, status) in [
                (http::Method::GET, "/api/v1/pods?watch=false", 403),
                (http::Method::GET, "/api/v1/pods?unwatch=true", 403),
                (http::Method::GET, "/api/v1/pods?watch=true%26other", 403),
                (http::Method::POST, "/api/v1/pods?watch=true", 403),
                (http::Method::GET, "/api/v1/pods?watch=true", 200),
            ] {
                let inner = tower::service_fn(move |_: Request<()>| async move {
                    Ok::<_, std::convert::Infallible>(
                        Response::builder()
                            .status(status)
                            .body(Full::new(Bytes::from_static(b"ORIGINAL BODY")))
                            .unwrap_or_else(|error| panic!("{error}")),
                    )
                });
                let request = Request::builder()
                    .method(method)
                    .uri(uri)
                    .body(())
                    .map_err(|_| "invalid fixture request")?;
                let response = BodyBudgetLayer::default()
                    .layer(inner)
                    .oneshot(request)
                    .await
                    .map_err(|_| "fixture transport failed")?;
                assert_eq!(response.status().as_u16(), status);
                let bytes = response
                    .into_body()
                    .collect()
                    .await
                    .map_err(|_| "fixture body read failed")?
                    .to_bytes();
                assert_eq!(bytes.as_ref(), b"ORIGINAL BODY", "{uri}");
            }
            Ok(())
        })
    }
    #[test]
    fn only_explicit_pod_log_follow_receives_stream_budget() -> crate::Result<()> {
        crate::cli::runtime()?.block_on(async {
            for (uri, allowed) in [
                ("/api/v1/namespaces/test/pods/app/log?follow=true", true),
                (
                    "/api/v1/namespaces/test/pods/app/log?follow=true&timestamps=true",
                    true,
                ),
                ("/api/v1/namespaces/test/pods/app/log?follow=false", false),
                ("/api/v1/namespaces/test/pods/app/log?unfollow=true", false),
                ("/api/v1/namespaces/test/configmaps?follow=true", false),
                ("/api/v1/namespaces/test/pods/app/log", false),
            ] {
                let inner = tower::service_fn(|_: Request<()>| async {
                    Ok::<_, std::convert::Infallible>(Response::new(Full::new(Bytes::from_static(
                        b"123456789",
                    ))))
                });
                let service = BodyBudgetLayer {
                    objects: 8,
                    logs: 16,
                }
                .layer(inner);
                let request = Request::builder()
                    .uri(uri)
                    .body(())
                    .map_err(|_| "invalid fixture request")?;
                let result = service
                    .oneshot(request)
                    .await
                    .map_err(|_| "fixture transport failed")?;
                assert_eq!(result.into_body().collect().await.is_ok(), allowed, "{uri}");
            }
            Ok(())
        })
    }
    #[test]
    fn quiet_object_body_stops_but_log_body_obeys_external_deadline() -> crate::Result<()> {
        struct Quiet;
        impl Body for Quiet {
            type Data = Bytes;
            type Error = std::convert::Infallible;
            fn poll_frame(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
            ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
                Poll::Pending
            }
        }
        crate::cli::runtime()?.block_on(async {
            let ordinary = IdleBody::new(Quiet, Some(Duration::from_millis(20)));
            assert!(ordinary.collect().await.is_err());
            let logs = IdleBody::new(Quiet, None);
            assert!(
                tokio::time::timeout(Duration::from_millis(40), logs.collect())
                    .await
                    .is_err()
            );
            Ok(())
        })
    }
}
