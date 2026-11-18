//! Release JSON, semver comparison and asset selection.
use crate::errors::UpdateError;
use serde::Deserialize;
use ulak_proto::VersionInfo;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SemVer(pub u64, pub u64, pub u64);

impl SemVer {
    /// `1.2.3` or `v1.2.3`; anything else (prerelease, build metadata, 4 parts) is `None`.
    pub fn parse(s: &str) -> Option<Self> {
        let mut it = s.strip_prefix('v').unwrap_or(s).split('.');
        let mut n = || {
            let p = it.next()?;
            (!p.is_empty() && p.len() <= 9 && p.bytes().all(|b| b.is_ascii_digit()))
                .then(|| p.parse::<u64>().ok())
                .flatten()
        };
        let v = Self(n()?, n()?, n()?);
        it.next().is_none().then_some(v)
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    /// API URL of the asset (fetched with `Accept: application/octet-stream`).
    pub url: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub version: SemVer,
    pub assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Raw {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

/// `None` for drafts, prereleases and tags that are not plain `vX.Y.Z`.
pub fn parse_release(json: &str) -> Result<Option<Release>, UpdateError> {
    let r: Raw = serde_json::from_str(json).map_err(|e| UpdateError::Release(e.to_string()))?;
    if r.draft || r.prerelease {
        return Ok(None);
    }
    Ok(SemVer::parse(&r.tag_name)
        .filter(|_| r.tag_name.starts_with('v'))
        .map(|version| Release {
            tag: r.tag_name,
            version,
            assets: r.assets,
        }))
}

/// The running version; an unparseable one cannot be compared, so updates are refused.
pub fn running(info: &VersionInfo) -> Result<SemVer, UpdateError> {
    SemVer::parse(info.version()).ok_or_else(|| {
        UpdateError::Release(format!("cannot parse own version {:?}", info.version()))
    })
}

/// Strictly newer only: never a downgrade, never the same version.
pub fn is_newer(cur: SemVer, rel: &Release) -> bool {
    rel.version > cur
}

/// Release target triple from the build's arch/os: `x86_64-unknown-linux-gnu`,
/// `x86_64-pc-windows-msvc`, `aarch64-apple-darwin`.
pub fn target(info: &VersionInfo) -> Result<String, UpdateError> {
    let vendor_os = match info.os() {
        "linux" => "unknown-linux-gnu",
        "windows" => "pc-windows-msvc",
        "macos" => "apple-darwin",
        os => {
            return Err(UpdateError::Release(format!(
                "self-update is not supported on {os}"
            )))
        }
    };
    Ok(format!("{}-{vendor_os}", info.arch()))
}

/// The binary inside `<product>-<target>.tar.gz`: `ulak.exe` on Windows, else the product name.
pub fn binary_name(product: &str, target: &str) -> String {
    if target.contains("windows") {
        format!("{product}.exe")
    } else {
        product.to_owned()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Picked<'a> {
    pub tarball_name: String,
    pub tarball: &'a Asset,
    pub tarball_sig: &'a Asset,
    pub sums: &'a Asset,
    pub sums_sig: &'a Asset,
}

/// `product` is `ulakd` or `ulak`: the archive is `<product>-<target>.tar.gz`.
pub fn select<'a>(
    rel: &'a Release,
    product: &str,
    target: &str,
) -> Result<Picked<'a>, UpdateError> {
    let find = |name: &str| {
        rel.assets
            .iter()
            .find(|a| a.name == name)
            .ok_or_else(|| UpdateError::NoAsset(name.to_owned()))
    };
    let tarball_name = format!("{product}-{target}.tar.gz");
    Ok(Picked {
        tarball: find(&tarball_name)?,
        tarball_sig: find(&format!("{tarball_name}.sig"))?,
        sums: find("SHA256SUMS")?,
        sums_sig: find("SHA256SUMS.sig")?,
        tarball_name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(tag: &str, extra: &str) -> String {
        format!(r#"{{"tag_name":"{tag}","assets":[]{extra}}}"#)
    }

    #[test]
    fn semver_parse_and_order() {
        assert_eq!(SemVer::parse("v1.2.3"), Some(SemVer(1, 2, 3)));
        assert_eq!(SemVer::parse("0.1.0"), Some(SemVer(0, 1, 0)));
        for bad in [
            "v1.2",
            "v1.2.3.4",
            "v1.2.3-rc1",
            "v1.2.3+b",
            "va.b.c",
            "v1..3",
            "",
            "v-1.0.0",
        ] {
            assert_eq!(SemVer::parse(bad), None, "{bad}");
        }
        assert!(SemVer(1, 10, 0) > SemVer(1, 9, 9)); // numeric, not lexical
        assert!(SemVer(2, 0, 0) > SemVer(1, 99, 99));
    }

    #[test]
    fn newer_never_downgrades() {
        let r = |t: &str| parse_release(&rel(t, "")).unwrap().unwrap();
        assert!(is_newer(SemVer(0, 1, 0), &r("v0.1.1")));
        assert!(!is_newer(SemVer(0, 1, 0), &r("v0.1.0")));
        assert!(!is_newer(SemVer(0, 2, 0), &r("v0.1.9")));
    }

    #[test]
    fn drafts_prereleases_and_odd_tags_are_ignored() {
        assert!(parse_release(&rel("v1.0.0", r#","draft":true"#))
            .unwrap()
            .is_none());
        assert!(parse_release(&rel("v1.0.0", r#","prerelease":true"#))
            .unwrap()
            .is_none());
        assert!(parse_release(&rel("v1.0.0-rc1", "")).unwrap().is_none());
        assert!(parse_release(&rel("1.0.0", "")).unwrap().is_none());
        assert!(parse_release("not json").is_err());
    }

    #[test]
    fn selects_the_platform_assets() {
        let a = |n: &str| format!(r#"{{"name":"{n}","url":"http://x/{n}","size":5}}"#);
        let names = [
            "ulak-remote-x86_64-unknown-linux-gnu.tar.gz",
            "ulakd-x86_64-unknown-linux-gnu.tar.gz",
            "ulakd-x86_64-unknown-linux-gnu.tar.gz.sig",
            "SHA256SUMS",
            "SHA256SUMS.sig",
        ];
        let assets = names.iter().map(|n| a(n)).collect::<Vec<_>>().join(",");
        let json = format!(r#"{{"tag_name":"v1.0.0","assets":[{assets}]}}"#);
        let r = parse_release(&json).unwrap().unwrap();
        let p = select(&r, "ulakd", "x86_64-unknown-linux-gnu").unwrap();
        assert_eq!(p.tarball.name, "ulakd-x86_64-unknown-linux-gnu.tar.gz");
        assert_eq!(p.sums_sig.name, "SHA256SUMS.sig");
        assert!(matches!(
            select(&r, "ulakd", "aarch64-unknown-linux-gnu"),
            Err(UpdateError::NoAsset(n)) if n == "ulakd-aarch64-unknown-linux-gnu.tar.gz"
        ));
        let mut missing = r.clone();
        missing.assets.retain(|a| a.name != "SHA256SUMS.sig");
        assert!(select(&missing, "ulakd", "x86_64-unknown-linux-gnu").is_err());
    }

    #[test]
    fn targets_and_binary_names() {
        let t =
            |os: &str, arch: &str| target(&VersionInfo::new("1.0.0", "g", 1, os, arch).unwrap());
        assert_eq!(t("linux", "x86_64").unwrap(), "x86_64-unknown-linux-gnu");
        assert_eq!(t("linux", "aarch64").unwrap(), "aarch64-unknown-linux-gnu");
        assert_eq!(t("macos", "aarch64").unwrap(), "aarch64-apple-darwin");
        assert_eq!(t("windows", "x86_64").unwrap(), "x86_64-pc-windows-msvc");
        assert!(t("freebsd", "x86_64").is_err());
        assert_eq!(binary_name("ulak", "x86_64-pc-windows-msvc"), "ulak.exe");
        assert_eq!(binary_name("ulakd", "aarch64-unknown-linux-gnu"), "ulakd");
    }
}
