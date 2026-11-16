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

/// `x86_64-unknown-linux-gnu` from the build's arch/os (Linux only).
pub fn target(info: &VersionInfo) -> Result<String, UpdateError> {
    if info.os() != "linux" {
        return Err(UpdateError::Release(format!(
            "self-update is not supported on {}",
            info.os()
        )));
    }
    Ok(format!("{}-unknown-linux-gnu", info.arch()))
}

#[derive(Debug, PartialEq, Eq)]
pub struct Picked<'a> {
    pub tarball_name: String,
    pub tarball: &'a Asset,
    pub tarball_sig: &'a Asset,
    pub sums: &'a Asset,
    pub sums_sig: &'a Asset,
}

pub fn select<'a>(rel: &'a Release, target: &str) -> Result<Picked<'a>, UpdateError> {
    let find = |name: &str| {
        rel.assets
            .iter()
            .find(|a| a.name == name)
            .ok_or_else(|| UpdateError::NoAsset(name.to_owned()))
    };
    let tarball_name = format!("ulakd-{target}.tar.gz");
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
        let p = select(&r, "x86_64-unknown-linux-gnu").unwrap();
        assert_eq!(p.tarball.name, "ulakd-x86_64-unknown-linux-gnu.tar.gz");
        assert_eq!(p.sums_sig.name, "SHA256SUMS.sig");
        assert!(matches!(
            select(&r, "aarch64-unknown-linux-gnu"),
            Err(UpdateError::NoAsset(n)) if n == "ulakd-aarch64-unknown-linux-gnu.tar.gz"
        ));
        let mut missing = r.clone();
        missing.assets.retain(|a| a.name != "SHA256SUMS.sig");
        assert!(select(&missing, "x86_64-unknown-linux-gnu").is_err());
    }

    #[test]
    fn target_is_linux_only() {
        let l = VersionInfo::new("1.0.0", "g", 1, "linux", "x86_64").unwrap();
        assert_eq!(target(&l).unwrap(), "x86_64-unknown-linux-gnu");
        let m = VersionInfo::new("1.0.0", "g", 1, "macos", "aarch64").unwrap();
        assert!(target(&m).is_err());
    }
}
