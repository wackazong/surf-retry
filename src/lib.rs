#![forbid(unsafe_code, future_incompatible)]
#![deny(
    missing_docs,
    missing_debug_implementations,
    missing_copy_implementations,
    nonstandard_style,
    unused_qualifications,
    unused_import_braces,
    unused_extern_crates,
    trivial_casts,
    trivial_numeric_casts
)]
#![cfg_attr(docsrs, feature(doc_cfg))]
//! A [surf] middleware that handles request retry logic
//! # Example
//! ```no_run
//! use surf_retry::{ExponentialBackoff, RetryMiddleware};
//! use surf_governor::GovernorMiddleware;
//! use surf::{Client, Request, http::Method};
//! use url::Url;
//!
//! #[async_std::main]
//! async fn main() -> surf::Result<()> {
//!     let req = Request::new(Method::Get, Url::parse("https://example.api")?);
//!     // Construct the retry middleware with max retries set to 3, exponential backoff also set to a max of 3, and a fallback interval of 1 second
//!     let retry = RetryMiddleware::new(
//!        3,
//!        ExponentialBackoff::builder().build_with_max_retries(3),
//!        1,
//!        );
//!     // Construct Surf client with the retry middleware and a limit of 1 request per second to force a retry
//!     let client = Client::new().with(retry).with(GovernorMiddleware::per_second(1)?);
//!     let res = client.send(req).await?;
//!     Ok(())
//! }
//! ```
//! # Features
//!
//! Per default `async_std` is used for waiting in between retries. To enable `tokio`, the default features must be disabled. The `tokio` feature
//! enables compilation as wasm. The same feature is also available as `wasm`.
use chrono::Utc;
use httpdate::parse_http_date;
pub use retry_policies::{policies::ExponentialBackoff, RetryPolicy};
use std::time::{Duration, SystemTime};
use surf::{
    http::{headers, StatusCode},
    middleware::{Middleware, Next},
    Client, Request, Response, Result,
};

/// The middleware is constructed with settings to handle a few different situations.
///
/// `max_retries` specifies the total number of attempts that will be made given a [`Retry-After`](https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Retry-After) header has been provided.
///
/// If no `Retry-After` header has been provided the configured [policy](https://docs.rs/retry-policies) will be used.
///
/// Should conditions for a retry be met but a retry interval failed to be determined the provided `fallback_interval` will be used.
#[derive(Debug)]
pub struct RetryMiddleware<T: RetryPolicy + Send + Sync + 'static> {
    max_retries: u32,
    policy: T,
    fallback_interval: u64,
}

impl Default for RetryMiddleware<ExponentialBackoff> {
    fn default() -> Self {
        Self::new(
            3,
            ExponentialBackoff::builder().build_with_max_retries(3),
            1,
        )
    }
}

impl<T: RetryPolicy + Send + Sync + 'static> RetryMiddleware<T> {
    /// Construct the retry middleware with provided options.
    pub fn new(max_retries: u32, policy: T, fallback_interval: u64) -> Self {
        Self {
            max_retries,
            policy,
            fallback_interval,
        }
    }

    fn use_policy(&self, retry_count: u32) -> u64 {
        let should_retry = self.policy.should_retry(retry_count);
        if let retry_policies::RetryDecision::Retry { execute_after } = should_retry {
            match (execute_after - Utc::now()).to_std() {
                Ok(duration) => duration.as_secs(),
                Err(_) => self.fallback_interval,
            }
        } else {
            self.fallback_interval
        }
    }
}

const RETRY_CODES: &[StatusCode] = &[StatusCode::TooManyRequests, StatusCode::RequestTimeout];

fn retry_to_seconds(header: &headers::HeaderValue) -> Result<u64> {
    let mut secs = match header.as_str().parse::<u64>() {
        Ok(s) => s,
        Err(_) => {
            let date = parse_http_date(header.as_str())?;
            let sys_time = SystemTime::now();
            let difference = date.duration_since(sys_time)?;
            difference.as_secs()
        }
    };
    if secs < 1 {
        secs = 1;
    }
    Ok(secs)
}

#[surf::utils::async_trait]
impl<T: RetryPolicy + Send + Sync + 'static> Middleware for RetryMiddleware<T> {
    async fn handle(&self, req: Request, client: Client, next: Next<'_>) -> Result<Response> {
        let mut retries: u32 = 0;

        let r: Request = req.clone();
        let res = next.run(r, client.clone()).await?;
        if RETRY_CODES.contains(&res.status()) {
            while retries < self.max_retries {
                retries += 1;

                let secs: u64;
                if let Some(retry_after) = res.header(headers::RETRY_AFTER) {
                    match retry_to_seconds(retry_after) {
                        Ok(s) => {
                            secs = s;
                        }
                        Err(_e) => {
                            secs = self.use_policy(retries);
                        }
                    };
                } else {
                    secs = self.use_policy(retries);
                };

                #[cfg(all(feature = "async-std", feature = "tokio"))]
                compile_error!("feature \"async-std\" and feature \"tokio\" cannot be enabled at the same time");

                #[cfg(all(feature = "async-std", feature = "wasm"))]
                compile_error!("feature \"async-std\" and feature \"tokio\" cannot be enabled at the same time");

                #[cfg(feature = "async-std")]
                async_std::task::sleep(Duration::from_secs(secs)).await;

                #[cfg(any(feature = "tokio", feature = "wasm"))]
                tokio::time::sleep(Duration::from_secs(secs)).await;

                let r: Request = req.clone();
                let res = next.run(r, client.clone()).await?;
                if !RETRY_CODES.contains(&res.status()) {
                    return Ok(res);
                }
            }
        }
        Ok(res)
    }
}

#[cfg(test)]
mod tests {
    use crate::*;
    use surf::{http::Method, Client, Request};
    use surf_governor::GovernorMiddleware;
    use url::Url;
    use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};

    #[async_std::test]
    async fn will_retry_request() -> Result<()> {
        let mock_server = MockServer::start().await;
        let m = Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("Hello!".to_string()))
            .expect(2);
        let _mock_guard = mock_server.register_as_scoped(m).await;
        let url = format!("{}/", &mock_server.uri());
        let req = Request::new(Method::Get, Url::parse(&url).unwrap());
        let retry = RetryMiddleware::default();
        let client = Client::new()
            .with(retry)
            .with(GovernorMiddleware::per_second(1)?);
        let good_res = client.send(req.clone()).await?;
        assert_eq!(good_res.status(), 200);
        let wait_res = client.send(req).await?;
        assert_eq!(wait_res.status(), 200);
        Ok(())
    }
}
