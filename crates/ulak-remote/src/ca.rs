use crate::EnrollError;
use rcgen::{
    date_time_ymd, BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair, KeyUsagePurpose, SubjectPublicKeyInfo,
};
use sha2::{Digest, Sha256};
use std::{io::Write, os::unix::fs::OpenOptionsExt, path::Path};
use ulak_proto::NodeId;

const SKEW_DAYS: i64 = 1;

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The hub's private CA. Signs client certificates for enrolled devices.
pub struct Ca {
    issuer: Issuer<'static, KeyPair>,
    pem: String,
    fingerprint: String,
}

impl Ca {
    /// Files `ca.pem` and `ca.key` (mode 0600) in `dir`; created on first use.
    pub fn load_or_create(dir: &Path) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        std::fs::create_dir_all(dir)?;
        let (pem_path, key_path) = (dir.join("ca.pem"), dir.join("ca.key"));
        if !pem_path.exists() || !key_path.exists() {
            let mut params = CertificateParams::new(Vec::<String>::new())?;
            params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
            params
                .distinguished_name
                .push(DnType::CommonName, "ulak hub CA");
            params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
            params.not_before = date_time_ymd(2025, 1, 1);
            params.not_after = date_time_ymd(2045, 1, 1);
            let key = KeyPair::generate()?;
            let cert = params.self_signed(&key)?;
            std::fs::write(&pem_path, cert.pem())?;
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&key_path)?
                .write_all(key.serialize_pem().as_bytes())?;
        }
        let pem = std::fs::read_to_string(&pem_path)?;
        let key = KeyPair::from_pem(&std::fs::read_to_string(&key_path)?)?;
        let (_, der) = x509_parser::pem::parse_x509_pem(pem.as_bytes())?;
        Ok(Self {
            fingerprint: hex(&Sha256::digest(&der.contents)),
            issuer: Issuer::from_ca_cert_pem(&pem, key)?,
            pem,
        })
    }

    pub fn cert_pem(&self) -> &str {
        &self.pem
    }

    /// SHA-256 of the CA certificate, hex. Shown next to the OTP so a new device can check the hub.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// TLS server certificate for the hub (1 year, in memory) for the given DNS names / IPs.
    /// Returns `(cert_pem, key_pem)`.
    pub fn issue_server_cert(
        &self,
        names: &[String],
        now_ms: u64,
    ) -> Result<(String, String), rcgen::Error> {
        let at = |days: i64| {
            time::OffsetDateTime::from_unix_timestamp((now_ms / 1000) as i64 + days * 86_400)
                .expect("timestamp is within the supported range")
        };
        let mut params = CertificateParams::new(names.to_vec())?;
        params
            .distinguished_name
            .push(DnType::CommonName, "ulak hub");
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        params.not_before = at(-SKEW_DAYS);
        params.not_after = at(365);
        let key = KeyPair::generate()?;
        let cert = params.signed_by(&key, &self.issuer)?;
        Ok((cert.pem(), key.serialize_pem()))
    }

    /// Client-auth certificate for `id`, valid `days` (backdated one day for clock skew).
    pub(crate) fn sign(
        &self,
        id: &NodeId,
        key: &SubjectPublicKeyInfo,
        now_ms: u64,
        days: i64,
    ) -> Result<String, EnrollError> {
        let at = |days: i64| {
            time::OffsetDateTime::from_unix_timestamp((now_ms / 1000) as i64 + days * 86_400)
                .expect("timestamp is within the supported range")
        };
        let mut params = CertificateParams::new(Vec::<String>::new())?;
        params
            .distinguished_name
            .push(DnType::CommonName, id.as_str());
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        params.not_before = at(-SKEW_DAYS);
        params.not_after = at(days);
        Ok(params.signed_by(key, &self.issuer)?.pem())
    }
}
