//! Signature, checksum and archive checks. Nothing here touches the installed binary.
use crate::errors::UpdateError;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};
use std::io::Read;

pub const MAX_UNPACKED: u64 = 300 * 1024 * 1024;

/// Raw 64-byte ed25519 signature over the file bytes (`openssl pkeyutl -sign -rawin`).
pub fn verify_sig(
    key: &VerifyingKey,
    data: &[u8],
    sig: &[u8],
    what: &str,
) -> Result<(), UpdateError> {
    let bad = || UpdateError::BadSignature(what.to_owned());
    let sig = Signature::from_slice(sig).map_err(|_| bad())?;
    key.verify(data, &sig).map_err(|_| bad())
}

pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `hash  name` lines as written by `sha256sum` (an optional `*` before the name is binary mode).
pub fn expected_hash<'a>(sums: &'a str, name: &str) -> Option<&'a str> {
    sums.lines().find_map(|l| {
        let (h, n) = l.split_once(char::is_whitespace)?;
        (n.trim_start().trim_start_matches('*') == name).then_some(h)
    })
}

/// Both signatures first, then the checksum, as the release pipeline signs both files.
#[allow(clippy::too_many_arguments)]
pub fn verify_release(
    key: &VerifyingKey,
    name: &str,
    tarball: &[u8],
    tarball_sig: &[u8],
    sums: &[u8],
    sums_sig: &[u8],
) -> Result<(), UpdateError> {
    verify_sig(key, sums, sums_sig, "SHA256SUMS")?;
    verify_sig(key, tarball, tarball_sig, name)?;
    let sums = std::str::from_utf8(sums).map_err(|_| UpdateError::BadChecksum(name.to_owned()))?;
    match expected_hash(sums, name) {
        Some(h) if h.eq_ignore_ascii_case(&sha256_hex(tarball)) => Ok(()),
        _ => Err(UpdateError::BadChecksum(name.to_owned())),
    }
}

/// The contents of the single regular file `name` (e.g. `ulakd`, `ulak.exe`) in the tarball. Any
/// other entry (a second file, directory, symlink, hard link, `..` or absolute path, other name)
/// rejects the archive.
pub fn extract_binary(tar_gz: &[u8], name: &str) -> Result<Vec<u8>, UpdateError> {
    let bad = |m: &str| UpdateError::Archive(m.to_owned());
    let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(tar_gz));
    let mut out = None;
    for e in ar.entries().map_err(|e| bad(&e.to_string()))? {
        let mut e = e.map_err(|e| bad(&e.to_string()))?;
        let path = e.path().map_err(|e| bad(&e.to_string()))?.into_owned();
        if !e.header().entry_type().is_file() {
            return Err(bad("entry is not a regular file"));
        }
        if path != std::path::Path::new(name) || out.is_some() {
            return Err(bad(&format!("unexpected entry {}", path.display())));
        }
        let mut buf = Vec::new();
        (&mut e)
            .take(MAX_UNPACKED + 1)
            .read_to_end(&mut buf)
            .map_err(|err| bad(&err.to_string()))?;
        if buf.len() as u64 > MAX_UNPACKED {
            return Err(bad("unpacked binary is too large"));
        }
        out = Some(buf);
    }
    out.filter(|b| !b.is_empty())
        .ok_or_else(|| bad(&format!("no {name} file in the archive")))
}

#[cfg(test)]
pub(crate) mod testutil {
    use ed25519_dalek::{Signer, SigningKey};

    pub fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }
    pub fn sign(k: &SigningKey, data: &[u8]) -> Vec<u8> {
        k.sign(data).to_bytes().to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::{testutil::*, *};

    fn tarball(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (n, d) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_size(d.len() as u64);
            h.set_mode(0o755);
            h.set_cksum();
            b.append_data(&mut h, n, *d).unwrap();
        }
        let raw = b.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &raw).unwrap();
        gz.finish().unwrap()
    }

    #[test]
    fn signature_and_checksum_roundtrip_and_failures() {
        let k = key(1);
        let vk = k.verifying_key();
        let name = "ulakd-x86_64-unknown-linux-gnu.tar.gz";
        let tgz = b"tarball bytes".to_vec();
        let sums = format!(
            "{}  {name}\n{}  other.tar.gz\n",
            sha256_hex(&tgz),
            "0".repeat(64)
        );
        let (ts, ss) = (sign(&k, &tgz), sign(&k, sums.as_bytes()));
        verify_release(&vk, name, &tgz, &ts, sums.as_bytes(), &ss).unwrap();

        let other = key(2);
        assert!(matches!(
            verify_release(
                &other.verifying_key(),
                name,
                &tgz,
                &ts,
                sums.as_bytes(),
                &ss
            ),
            Err(UpdateError::BadSignature(_))
        ));
        let mut tampered = tgz.clone();
        tampered[0] ^= 1;
        assert!(matches!(
            verify_release(&vk, name, &tampered, &ts, sums.as_bytes(), &ss),
            Err(UpdateError::BadSignature(n)) if n == name
        ));
        assert!(matches!(
            verify_release(&vk, name, &tgz, &ts[..63], sums.as_bytes(), &ss),
            Err(UpdateError::BadSignature(_))
        ));
        // sums validly signed but listing the wrong hash, or not listing the file
        let wrong = format!("{}  {name}\n", "1".repeat(64));
        let ws = sign(&k, wrong.as_bytes());
        assert!(matches!(
            verify_release(&vk, name, &tgz, &ts, wrong.as_bytes(), &ws),
            Err(UpdateError::BadChecksum(_))
        ));
        let missing = "0".repeat(64) + "  other\n";
        let ms = sign(&k, missing.as_bytes());
        assert!(matches!(
            verify_release(&vk, name, &tgz, &ts, missing.as_bytes(), &ms),
            Err(UpdateError::BadChecksum(_))
        ));
        // a forged SHA256SUMS signature
        assert!(matches!(
            verify_release(&vk, name, &tgz, &ts, wrong.as_bytes(), &ss),
            Err(UpdateError::BadSignature(n)) if n == "SHA256SUMS"
        ));
    }

    #[test]
    fn expected_hash_formats() {
        assert_eq!(expected_hash("abc  f.tgz\n", "f.tgz"), Some("abc"));
        assert_eq!(expected_hash("abc *f.tgz\n", "f.tgz"), Some("abc"));
        assert_eq!(expected_hash("abc  xf.tgz\n", "f.tgz"), None);
    }

    #[test]
    fn extracts_only_the_single_expected_file() {
        assert_eq!(
            extract_binary(&tarball(&[("ulakd", b"BIN")]), "ulakd").unwrap(),
            b"BIN"
        );
        for bad in [
            tarball(&[("evil", b"x")]),
            tarball(&[("ulakd", b"a"), ("ulakd", b"b")]),
            tarball(&[("ulakd", b"a"), ("extra", b"b")]),
            tarball(&[("dir/ulakd", b"a")]),
            tarball(&[("ulakd", b"")]),
            tarball(&[]),
            b"not gzip".to_vec(),
        ] {
            assert!(matches!(
                extract_binary(&bad, "ulakd"),
                Err(UpdateError::Archive(_))
            ));
        }
    }

    #[test]
    fn rejects_traversal_and_links() {
        // The tar crate's builder refuses `..`, so write those headers by hand.
        let raw = |name: &[u8], kind: u8, link: &[u8]| {
            let mut h = tar::Header::new_gnu();
            h.set_size(0);
            h.set_entry_type(tar::EntryType::new(kind));
            {
                let b = h.as_old_mut();
                b.name[..name.len()].copy_from_slice(name);
                b.linkname[..link.len()].copy_from_slice(link);
            }
            h.set_cksum();
            let mut v = h.as_bytes().to_vec();
            v.extend_from_slice(&[0; 1024]);
            let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            std::io::Write::write_all(&mut gz, &v).unwrap();
            gz.finish().unwrap()
        };
        for t in [
            raw(b"../ulakd", b'0', b""),
            raw(b"/etc/ulakd", b'0', b""),
            raw(b"ulakd", b'2', b"/etc/passwd"), // symlink named like the binary
            raw(b"ulakd", b'1', b"/etc/passwd"), // hard link
            raw(b"ulakd", b'5', b""),            // directory
        ] {
            assert!(matches!(
                extract_binary(&t, "ulakd"),
                Err(UpdateError::Archive(_))
            ));
        }
    }
}
