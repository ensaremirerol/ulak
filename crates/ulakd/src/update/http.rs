//! Blocking HTTP (ureq, rustls) for the GitHub API; call from `spawn_blocking`.
use super::trust::ALLOW_HTTP;
use crate::errors::UpdateError;
use std::time::Duration;

const MAX_REDIRECTS: usize = 5;

pub struct Http {
    agent: ureq::Agent,
    /// Test builds only: make `get` panic (proves the updater survives a dying http task).
    #[cfg(feature = "test-key")]
    panics: bool,
}

impl Default for Http {
    fn default() -> Self {
        Self::new()
    }
}

/// `scheme://host[:port]` of an absolute URL.
pub fn origin(url: &str) -> Option<&str> {
    let (_, rest) = url.split_once("://")?;
    let end = rest
        .find('/')
        .map_or(url.len(), |i| url.len() - rest.len() + i);
    Some(&url[..end])
}

impl Http {
    pub fn new() -> Self {
        Self::build(None)
    }

    /// Test builds only: trust exactly this DER root (a local test CA) instead of the Mozilla roots.
    #[cfg(feature = "test-key")]
    pub fn with_test_root(der: &[u8]) -> Self {
        Self::build(Some(ureq::tls::Certificate::from_der(der).to_owned()))
    }

    /// Test builds only: an http backend whose task panics.
    #[cfg(feature = "test-key")]
    pub fn panicking() -> Self {
        Self {
            panics: true,
            ..Self::new()
        }
    }

    fn build(test_root: Option<ureq::tls::Certificate<'static>>) -> Self {
        // Roots are explicit: ureq's `rustls-no-provider` has no implicit store, and the bundled
        // Mozilla set (`rustls-webpki-roots`) needs no system library or ca-certificates package.
        let roots = match test_root {
            Some(c) => ureq::tls::RootCerts::new_with_certs(&[c]),
            None => ureq::tls::RootCerts::WebPki,
        };
        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_global(Some(Duration::from_secs(600)))
            .max_redirects(0)
            // aws-lc-rs like the rest of ulakd: ring is not enabled, so rustls has no implicit default.
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .root_certs(roots)
                    .unversioned_rustls_crypto_provider(std::sync::Arc::new(
                        rustls::crypto::aws_lc_rs::default_provider(),
                    ))
                    .build(),
            )
            .http_status_as_error(false)
            .build()
            .into();
        Self {
            agent,
            #[cfg(feature = "test-key")]
            panics: false,
        }
    }

    /// GET with at most `max` body bytes. `token` goes into the first request only: redirects are
    /// followed here by hand and never carry it (a release asset redirects to another host).
    pub fn get(
        &self,
        url: &str,
        token: Option<&str>,
        accept: &str,
        max: u64,
    ) -> Result<Vec<u8>, UpdateError> {
        #[cfg(feature = "test-key")]
        assert!(!self.panics, "injected http panic");
        let mut url = url.to_owned();
        let mut token = token;
        for _ in 0..=MAX_REDIRECTS {
            if !(url.starts_with("https://") || (ALLOW_HTTP && url.starts_with("http://"))) {
                return Err(UpdateError::Http(format!(
                    "refusing non-https URL {}",
                    origin(&url).unwrap_or("?")
                )));
            }
            let mut req = self
                .agent
                .get(&url)
                .header("Accept", accept)
                .header("User-Agent", concat!("ulakd/", env!("CARGO_PKG_VERSION")))
                .header("X-GitHub-Api-Version", "2022-11-28");
            if let Some(t) = token {
                req = req.header("Authorization", format!("Bearer {t}"));
            }
            let mut resp = req
                .call()
                .map_err(|e| UpdateError::Http(redact(&e.to_string())))?;
            let status = resp.status().as_u16();
            if (300..400).contains(&status) {
                let loc = resp
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| {
                        UpdateError::Http(format!("redirect {status} without Location"))
                    })?;
                url = if loc.contains("://") {
                    loc.to_owned()
                } else {
                    format!("{}{loc}", origin(&url).unwrap_or(""))
                };
                token = None;
                continue;
            }
            if status != 200 {
                let hint = match status {
                    401 | 403 | 404 => " (repository not found, or a GitHub token is required)",
                    _ => "",
                };
                return Err(UpdateError::Http(format!(
                    "HTTP {status} from {}{hint}",
                    origin(&url).unwrap_or("?")
                )));
            }
            return resp
                .body_mut()
                .with_config()
                .limit(max)
                .read_to_vec()
                .map_err(|e| match e {
                    ureq::Error::BodyExceedsLimit(_) => UpdateError::TooLarge(max),
                    e => UpdateError::Http(format!("download failed: {}", redact(&e.to_string()))),
                });
        }
        Err(UpdateError::Http("too many redirects".into()))
    }
}

/// Error texts can echo URLs (signed asset links carry credentials in the query).
pub(crate) fn redact(s: &str) -> String {
    s.split_whitespace()
        .map(|w| w.split('?').next().unwrap_or(w))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_of_urls() {
        assert_eq!(
            origin("https://api.github.com/repos/x"),
            Some("https://api.github.com")
        );
        assert_eq!(origin("http://127.0.0.1:80"), Some("http://127.0.0.1:80"));
        assert_eq!(origin("nope"), None);
    }

    #[test]
    fn redact_drops_queries() {
        assert_eq!(
            redact("GET https://h/p?X-Amz-Signature=abc failed"),
            "GET https://h/p failed"
        );
    }
}
