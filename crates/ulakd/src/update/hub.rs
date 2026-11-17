//! Update source "hub": the hub caches the signed release and serves it to enrolled nodes over
//! mTLS. The hub is untrusted for integrity: everything it serves still goes through
//! `verify::verify_release` with the embedded release key.
use super::release::{Asset, Release, SemVer};
use crate::{config::Identity, errors::UpdateError};
use rustls::pki_types::{pem::PemObject, CertificateDer};
use serde::Deserialize;
use std::{path::Path, time::Duration};
use ureq::tls::{Certificate, ClientCert, PrivateKey, RootCerts, TlsConfig};

const NO_RELEASE: &str =
    "hub has no release cached yet: ask the admin to run `ulak-remote admin releases-refresh`";

fn unreachable_(why: &str) -> UpdateError {
    UpdateError::Hub(format!(
        "hub update source unreachable: {}",
        super::http::redact(why)
    ))
}

/// A blocking mTLS client built from the identity files as they are right now.
pub struct HubClient {
    agent: ureq::Agent,
    /// `https://host[:port]`, no trailing slash.
    pub base: String,
}

/// The hub's `GET /api/releases/latest` body.
#[derive(Deserialize)]
struct Listing {
    version: String,
    #[serde(default)]
    assets: Vec<ListedAsset>,
}

#[derive(Deserialize)]
struct ListedAsset {
    name: String,
    #[serde(default)]
    size: u64,
}

impl HubClient {
    /// Reads `hub_url`, CA, cert and key from `dir` (so a renewed certificate is used at once).
    pub fn load(dir: &Path) -> Result<Self, UpdateError> {
        let id = Identity::load(dir).map_err(|e| unreachable_(&e.to_string()))?;
        Self::from_identity(&id)
    }

    pub fn from_identity(id: &Identity) -> Result<Self, UpdateError> {
        let bad = |e: &dyn std::fmt::Display| unreachable_(&format!("bad identity: {e}"));
        let base = id.hub_url.trim().trim_end_matches('/').to_owned();
        if !base.starts_with("https://") || base[8..].contains('/') {
            return Err(unreachable_("hub_url must be https://host[:port]"));
        }
        // The hub CA is the only root: no Mozilla bundle, no system store.
        let roots = CertificateDer::pem_slice_iter(id.ca_pem.as_bytes())
            .map(|c| c.map(|c| Certificate::from_der(c.as_ref()).to_owned()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| bad(&e))?;
        if roots.is_empty() {
            return Err(bad(&"no CA certificate"));
        }
        let chain = CertificateDer::pem_slice_iter(id.cert_pem.as_bytes())
            .map(|c| c.map(|c| Certificate::from_der(c.as_ref()).to_owned()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| bad(&e))?;
        let key = PrivateKey::from_pem(id.key_pem.as_bytes()).map_err(|e| bad(&e))?;
        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_global(Some(Duration::from_secs(600)))
            .max_redirects(0)
            .tls_config(
                TlsConfig::builder()
                    .root_certs(RootCerts::new_with_certs(&roots))
                    .client_cert(Some(ClientCert::new_with_certs(&chain, key)))
                    .unversioned_rustls_crypto_provider(std::sync::Arc::new(
                        rustls::crypto::aws_lc_rs::default_provider(),
                    ))
                    .build(),
            )
            .http_status_as_error(false)
            .build()
            .into();
        Ok(Self { agent, base })
    }

    /// GET `path` on the hub with at most `max` body bytes. Never follows a redirect.
    pub fn get(&self, path: &str, max: u64) -> Result<Vec<u8>, UpdateError> {
        let mut resp = self
            .agent
            .get(format!("{}{path}", self.base))
            .header("User-Agent", concat!("ulakd/", env!("CARGO_PKG_VERSION")))
            .call()
            .map_err(|e| unreachable_(&e.to_string()))?;
        match resp.status().as_u16() {
            200 => {}
            404 => return Err(UpdateError::Hub(NO_RELEASE.into())),
            401 | 403 => return Err(UpdateError::Hub("hub refused the update request".into())),
            s => return Err(unreachable_(&format!("HTTP {s}"))),
        }
        resp.body_mut()
            .with_config()
            .limit(max)
            .read_to_vec()
            .map_err(|e| match e {
                ureq::Error::BodyExceedsLimit(_) => UpdateError::TooLarge(max),
                e => unreachable_(&e.to_string()),
            })
    }

    /// The hub's latest release as a `Release` whose asset URLs are paths on this hub.
    pub fn latest(&self) -> Result<Release, UpdateError> {
        let body = self.get("/api/releases/latest", 1024 * 1024)?;
        let l: Listing = serde_json::from_slice(&body)
            .map_err(|e| UpdateError::Release(format!("hub listing: {e}")))?;
        let version = SemVer::parse(&l.version)
            .filter(|_| !l.version.starts_with('v'))
            .ok_or_else(|| UpdateError::Release("hub listing: bad version".into()))?;
        let assets = l
            .assets
            .into_iter()
            .map(|a| Asset {
                url: format!("/api/releases/{}/{}", l.version, a.name),
                name: a.name,
                size: a.size,
            })
            .collect();
        Ok(Release {
            tag: format!("v{}", l.version),
            version,
            assets,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(hub_url: &str, ca: &str) -> Identity {
        Identity {
            hub_url: hub_url.into(),
            node_id: ulak_proto::NodeId::new("n1").unwrap(),
            ca_pem: ca.into(),
            cert_pem: String::new(),
            key_pem: String::new(),
            guard: Default::default(),
        }
    }

    #[test]
    fn client_config_rejects_http_paths_and_empty_ca() {
        for u in ["http://h:1", "https://h/x", "h"] {
            let e = HubClient::from_identity(&id(u, "")).err().unwrap();
            assert!(
                e.to_string().starts_with("hub update source unreachable"),
                "{u}"
            );
        }
        assert!(HubClient::from_identity(&id("https://h:1/", "")).is_err());
    }
}
