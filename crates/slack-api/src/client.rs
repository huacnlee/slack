//! The HTTP seam to `https://slack.com/api`.
//!
//! Transport lives on a private Tokio runtime so callers can `await` a request
//! from any executor — GPUI's included — without a reactor of their own. Every
//! request is handed to that runtime and its result comes back over a oneshot
//! channel, which is what makes [`SlackClient::call`] executor-agnostic.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures::channel::oneshot;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::{Error, Result};

const API_BASE: &str = "https://slack.com/api";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// Slack replies are small; a hostile or wedged endpoint must not stream
/// without end into this process.
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_RETRIES: u32 = 3;

/// How a request body should be encoded.
enum Body {
    /// GET with query parameters.
    Query(Vec<(String, String)>),
    /// POST with a JSON document.
    Json(Value),
    /// POST with form encoding, which a few older methods still require.
    Form(Vec<(String, String)>),
}

/// A Slack Web API client bound to one user token.
///
/// Cloning is cheap: clones share the runtime, the connection pool, and the
/// token, so a view can hold its own handle without duplicating any of it.
#[derive(Clone)]
pub struct SlackClient {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    /// Keeps every call site from flooding one method, whatever it asks for.
    limiter: crate::limiter::Limiter,
    runtime: tokio::runtime::Runtime,
    token: String,
    /// Requests issued since construction — surfaced for diagnostics only.
    requests: AtomicU64,
}

impl SlackClient {
    /// Build a client for `token`, which must be a user (`xoxp-`) or bot
    /// (`xoxb-`) token.
    pub fn new(token: impl Into<String>) -> Result<Self> {
        let token = token.into();
        if token.is_empty() {
            return Err(Error::NoToken);
        }

        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .https_only(true)
            .user_agent(concat!("slack-desktop/", env!("CARGO_PKG_VERSION")))
            .build()?;

        // Two worker threads are enough: this runtime only drives HTTP.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("slack-api")
            .enable_all()
            .build()
            .map_err(|e| Error::Other(format!("could not start the network runtime: {e}")))?;

        Ok(Self {
            inner: Arc::new(Inner {
                http,
                limiter: Default::default(),
                runtime,
                token,
                requests: AtomicU64::new(0),
            }),
        })
    }

    pub fn token(&self) -> &str {
        &self.inner.token
    }

    /// Run a long-lived task on the transport runtime.
    ///
    /// Requests bridge back over a oneshot and so need no handle; a socket
    /// outlives any single call and does, which is what this is for.
    pub(crate) fn spawn_on_transport<F>(&self, future: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        self.inner.runtime.spawn(future);
    }

    pub fn request_count(&self) -> u64 {
        self.inner.requests.load(Ordering::Relaxed)
    }

    /// `GET https://slack.com/api/{method}?…`
    pub async fn get<T: DeserializeOwned>(
        &self,
        method: &str,
        params: &[(&str, String)],
    ) -> Result<T> {
        let query = params
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect();
        self.call(method, Body::Query(query)).await
    }

    /// `POST https://slack.com/api/{method}` with a JSON body.
    pub async fn post_json<T: DeserializeOwned>(
        &self,
        method: &str,
        body: impl Serialize,
    ) -> Result<T> {
        let value = serde_json::to_value(body)?;
        self.call(method, Body::Json(value)).await
    }

    /// `POST` with `application/x-www-form-urlencoded`, for the methods that
    /// still reject JSON (`users.setPresence`, `dnd.setSnooze`, …).
    pub async fn post_form<T: DeserializeOwned>(
        &self,
        method: &str,
        params: &[(&str, String)],
    ) -> Result<T> {
        let form = params
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect();
        self.call(method, Body::Form(form)).await
    }

    /// Send raw bytes to a Slack-issued upload URL.
    ///
    /// The URL is pre-signed, so the token is deliberately not attached.
    pub async fn put_bytes(&self, url: &str, bytes: Vec<u8>) -> Result<()> {
        if !url.starts_with("https://") {
            return Err(Error::Other(
                "refusing to upload over a plain connection".into(),
            ));
        }

        let (tx, rx) = oneshot::channel();
        let inner = self.inner.clone();
        let url = url.to_string();

        self.inner.runtime.spawn(async move {
            let result = async {
                let response = inner.http.post(&url).body(bytes).send().await?;
                if !response.status().is_success() {
                    return Err(Error::Other(format!(
                        "the upload was rejected with HTTP {}",
                        response.status()
                    )));
                }
                Ok(())
            }
            .await;
            let _ = tx.send(result);
        });

        rx.await
            .map_err(|_| Error::Other("the upload was cancelled".into()))?
    }

    /// Fetch a private Slack asset (`url_private`, a thumbnail) with the
    /// token attached, which those URLs require.
    pub async fn download(&self, url: &str, max_bytes: usize) -> Result<Vec<u8>> {
        if !url.starts_with("https://") {
            return Err(Error::Other(
                "refusing to download over a plain connection".into(),
            ));
        }

        let (tx, rx) = oneshot::channel();
        let inner = self.inner.clone();
        let url = url.to_string();

        self.inner.runtime.spawn(async move {
            let result = async {
                let response = inner
                    .http
                    .get(&url)
                    .bearer_auth(&inner.token)
                    .send()
                    .await?;
                if !response.status().is_success() {
                    return Err(Error::Other(format!(
                        "Slack replied with HTTP {} for that file",
                        response.status()
                    )));
                }
                let bytes = response.bytes().await?;
                if bytes.len() > max_bytes {
                    return Err(Error::Other("that file is too large to preview".into()));
                }
                Ok(bytes.to_vec())
            }
            .await;
            let _ = tx.send(result);
        });

        rx.await
            .map_err(|_| Error::Other("the download was cancelled".into()))?
    }

    /// Issue one API call, retrying only on a rate limit or a transient
    /// network fault, and decode the `ok`-wrapped envelope.
    async fn call<T: DeserializeOwned>(&self, method: &str, body: Body) -> Result<T> {
        let value = self.call_raw(method, body).await?;
        serde_json::from_value(value).map_err(Error::from)
    }

    async fn call_raw(&self, method: &str, body: Body) -> Result<Value> {
        // The Slack method name becomes part of a URL; refuse anything that is
        // not a plain `namespace.method` identifier.
        if !is_valid_method(method) {
            return Err(Error::Other(format!("invalid API method: {method}")));
        }

        let (tx, rx) = oneshot::channel();
        let inner = self.inner.clone();
        let url = format!("{API_BASE}/{method}");
        let method = method.to_string();

        inner.requests.fetch_add(1, Ordering::Relaxed);
        let runtime = self.inner.runtime.handle().clone();
        runtime.spawn(async move {
            let result = execute(&inner, &method, &url, body).await;
            // A dropped receiver just means the caller lost interest.
            let _ = tx.send(result);
        });

        rx.await
            .map_err(|_| Error::Other("the request was cancelled".into()))?
    }
}

/// Send the request, honouring `Retry-After` and unwrapping Slack's envelope.
async fn execute(inner: &Inner, method: &str, url: &str, body: Body) -> Result<Value> {
    let mut attempt = 0;

    loop {
        // Wait for this method's turn before spending a request on it. Being
        // refused costs the whole window, so it is cheaper to be early.
        let wait = inner.limiter.reserve(method);
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }

        let request = match &body {
            Body::Query(params) => inner.http.get(url).query(params),
            Body::Json(value) => inner.http.post(url).json(value),
            Body::Form(params) => inner.http.post(url).form(params),
        }
        .bearer_auth(&inner.token);

        let response = match request.send().await {
            Ok(response) => response,
            Err(err) if err.is_timeout() || err.is_connect() => {
                attempt += 1;
                if attempt > MAX_RETRIES {
                    return Err(Error::Network(err));
                }
                tokio::time::sleep(backoff(attempt)).await;
                continue;
            }
            Err(err) => return Err(Error::Network(err)),
        };

        if response.status().as_u16() == 429 {
            let delay = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(30);

            // Slack has now told us how wide the window really is. Recording
            // it here is what stops the next hundred requests repeating this.
            inner
                .limiter
                .refused(method, Duration::from_secs(delay.min(60)));

            attempt += 1;
            if attempt > MAX_RETRIES {
                return Err(Error::RateLimited(delay));
            }
            // Worth reporting: a caller that sees a slow request wants to know
            // it is waiting on Slack's limiter, not on the network.
            log::warn!("{method} rate limited, pacing at {delay}s (attempt {attempt})");
            // The reservation above already waits out the window.
            continue;
        }

        if !response.status().is_success() {
            let status = response.status();
            return Err(Error::Other(format!("Slack replied with HTTP {status}")));
        }

        let bytes = response.bytes().await?;
        if bytes.len() > MAX_RESPONSE_BYTES {
            return Err(Error::Other("the reply from Slack was too large".into()));
        }

        let mut value: Value = serde_json::from_slice(&bytes)?;
        if value.get("ok").and_then(Value::as_bool) != Some(true) {
            let code = value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown_error")
                .to_string();

            // Slack also reports rate limits inside a 200 body.
            if code == "ratelimited" {
                attempt += 1;
                if attempt <= MAX_RETRIES {
                    log::warn!("{url} rate limited in body, retrying (attempt {attempt})");
                    tokio::time::sleep(backoff(attempt)).await;
                    continue;
                }
            }
            return Err(Error::Slack(code));
        }

        // `ok` has served its purpose; the typed shapes never declare it.
        if let Some(object) = value.as_object_mut() {
            object.remove("ok");
        }
        return Ok(value);
    }
}

fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(500 * 2u64.pow(attempt.min(5)))
}

fn is_valid_method(method: &str) -> bool {
    !method.is_empty()
        && method.contains('.')
        && method
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_method_names_that_are_not_identifiers() {
        assert!(is_valid_method("conversations.history"));
        assert!(is_valid_method("admin.conversations.archive"));
        assert!(!is_valid_method("conversations"));
        assert!(!is_valid_method("../../etc/passwd"));
        assert!(!is_valid_method("chat.postMessage?x=1"));
        assert!(!is_valid_method(""));
    }

    #[test]
    fn backoff_grows_and_stays_bounded() {
        assert!(backoff(1) < backoff(3));
        assert!(backoff(9) <= Duration::from_secs(30));
    }

    #[test]
    fn an_empty_token_is_refused_at_construction() {
        assert!(matches!(SlackClient::new(""), Err(Error::NoToken)));
    }
}
