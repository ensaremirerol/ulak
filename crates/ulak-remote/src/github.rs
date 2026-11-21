//! Blocking GitHub REST client (ureq). Call from `spawn_blocking`. The token is only ever put
//! in the Authorization header: never in a URL, an error message or a log line.
use crate::ServerError;
use std::{net::IpAddr, time::Duration};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use ulak_proto::{clean_text, BranchName, GitToken, Issue, IssueRef, RepoInfo, RepoRef};

const MAX_BODY: u64 = 2 << 20;
const PAGE: usize = 30;
const SCAN_PAGES: u32 = 10;

pub struct GitHub {
    agent: ureq::Agent,
    /// Same TLS setup, long timeout: release assets are tens of MiB.
    dl: ureq::Agent,
    base: String,
}

fn agent(global: u64) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_global(Some(Duration::from_secs(global)))
        .max_redirects(0)
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::WebPki)
                .unversioned_rustls_crypto_provider(std::sync::Arc::new(
                    tokio_rustls::rustls::crypto::aws_lc_rs::default_provider(),
                ))
                .build(),
        )
        .http_status_as_error(false)
        .build()
        .into()
}

fn origin(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    &url[..url.len() - rest.len() + rest.find('/').unwrap_or(rest.len())]
}

fn gh(m: impl Into<String>) -> ServerError {
    ServerError::GitHub(m.into())
}

/// https, or http for a loopback host only (tests, a local GitHub Enterprise proxy).
pub(crate) fn validate_api(base: &str) -> Result<String, ServerError> {
    let base = base.trim().trim_end_matches('/');
    let bad =
        || ServerError::BadArgs("github_api must be an https URL (http only for loopback)".into());
    let (scheme, rest) = base.split_once("://").ok_or_else(bad)?;
    let authority = rest.split('/').next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') {
        return Err(bad());
    }
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or(""),
        None => authority.split(':').next().unwrap_or(""),
    };
    let loopback = host == "localhost" || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback());
    match scheme {
        "https" => Ok(base.to_owned()),
        "http" if loopback => Ok(base.to_owned()),
        _ => Err(bad()),
    }
}

impl GitHub {
    pub fn new(base: &str) -> Result<Self, ServerError> {
        let base = validate_api(base)?;
        let (agent, dl) = (agent(20), agent(600));
        Ok(Self { agent, dl, base })
    }

    /// `(json, has_next_page)`.
    fn get(&self, path: &str, token: &GitToken) -> Result<(serde_json::Value, bool), ServerError> {
        self.request(path, token, None)
    }

    /// GET, or POST of a JSON `body` (which must answer 201).
    fn request(
        &self,
        path: &str,
        token: &GitToken,
        body: Option<&serde_json::Value>,
    ) -> Result<(serde_json::Value, bool), ServerError> {
        let url = format!("{}{path}", self.base);
        let auth = format!("Bearer {}", token.expose());
        let ua = concat!("ulak-remote/", env!("CARGO_PKG_VERSION"));
        let sent = match body {
            None => self
                .agent
                .get(&url)
                .header("Authorization", &auth)
                .header("User-Agent", ua)
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28")
                .call(),
            Some(b) => self
                .agent
                .post(&url)
                .header("Authorization", &auth)
                .header("User-Agent", ua)
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28")
                .header("Content-Type", "application/json")
                .send(b.to_string()),
        };
        let mut resp =
            sent.map_err(|_| gh("cannot reach the GitHub API (network error or timeout)"))?;
        let status = resp.status().as_u16();
        let h = |n: &str| {
            resp.headers()
                .get(n)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        let want = if body.is_some() { 201 } else { 200 };
        if status != want {
            return Err(gh(match status {
                401 => "token rejected (invalid, expired or revoked)".to_owned(),
                429 => "rate limit exceeded; try again later".to_owned(),
                403 if h("x-ratelimit-remaining").as_deref() == Some("0") => {
                    "rate limit exceeded; try again later".to_owned()
                }
                403 => "no access with this token".to_owned(),
                404 => "not found (or the token cannot see it)".to_owned(),
                n => format!("unexpected HTTP status {n}"),
            }));
        }
        let next = h("link").is_some_and(|l| l.contains("rel=\"next\""));
        let json = resp
            .body_mut()
            .with_config()
            .limit(MAX_BODY)
            .read_to_vec()
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .ok_or_else(|| gh("unreadable or oversized response"))?;
        Ok((json, next))
    }

    /// Validates the token; returns the account login.
    /// GET with at most `max` body bytes. The token goes only to the API host itself; redirects
    /// (assets live on a CDN host) are followed by hand and never carry it. Errors never contain
    /// the URL (signed asset links carry credentials in the query).
    fn fetch(
        &self,
        url: &str,
        token: Option<&GitToken>,
        accept: &str,
        max: u64,
    ) -> Result<Vec<u8>, ServerError> {
        let mut url = url.to_owned();
        let mut send_token = token.filter(|_| origin(&url) == origin(&self.base));
        for _ in 0..=5 {
            // Same rule as the API base: https, or http for loopback (tests).
            validate_api(origin(&url))?;
            let mut req = self
                .dl
                .get(&url)
                .header("Accept", accept)
                .header(
                    "User-Agent",
                    concat!("ulak-remote/", env!("CARGO_PKG_VERSION")),
                )
                .header("X-GitHub-Api-Version", "2022-11-28");
            if let Some(t) = send_token {
                req = req.header("Authorization", format!("Bearer {}", t.expose()));
            }
            let mut resp = req
                .call()
                .map_err(|_| gh("cannot reach GitHub (network error or timeout)"))?;
            let status = resp.status().as_u16();
            if (300..400).contains(&status) {
                let loc = resp
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| gh("redirect without Location"))?;
                url = if loc.contains("://") {
                    loc.to_owned()
                } else {
                    format!("{}{loc}", origin(&url))
                };
                send_token = None;
                continue;
            }
            if status != 200 {
                return Err(gh(match status {
                    401 | 403 | 404 => "HTTP 404 from GitHub: release not found, or a token is required (set ULAK_RELEASE_CREDENTIAL or ULAK_GH_TOKEN)".to_owned(),
                    429 => "rate limit exceeded; try again later".to_owned(),
                    n => format!("unexpected HTTP status {n}"),
                }));
            }
            return resp
                .body_mut()
                .with_config()
                .limit(max)
                .read_to_vec()
                .map_err(|e| match e {
                    ureq::Error::BodyExceedsLimit(_) => {
                        gh(format!("download larger than {max} bytes"))
                    }
                    _ => gh("download failed"),
                });
        }
        Err(gh("too many redirects"))
    }

    /// JSON of `releases/latest` of `repo` (`owner/name`).
    pub fn latest_release(
        &self,
        repo: &str,
        token: Option<&GitToken>,
    ) -> Result<String, ServerError> {
        let url = format!("{}/repos/{repo}/releases/latest", self.base);
        let b = self.fetch(&url, token, "application/vnd.github+json", MAX_BODY)?;
        String::from_utf8(b).map_err(|_| gh("unreadable response"))
    }

    /// One release asset by its API URL (must be on the API host).
    pub fn asset(
        &self,
        url: &str,
        token: Option<&GitToken>,
        max: u64,
    ) -> Result<Vec<u8>, ServerError> {
        if origin(url) != origin(&self.base) {
            return Err(gh("asset URL is not on the API host"));
        }
        self.fetch(url, token, "application/octet-stream", max)
    }

    pub fn login(&self, token: &GitToken) -> Result<String, ServerError> {
        let (j, _) = self.get("/user", token)?;
        j["login"]
            .as_str()
            .filter(|l| !l.is_empty() && l.len() <= 100 && !l.chars().any(char::is_control))
            .map(str::to_owned)
            .ok_or_else(|| gh("response has no login"))
    }

    fn page(
        &self,
        token: &GitToken,
        per: usize,
        page: u32,
    ) -> Result<(Vec<RepoInfo>, bool), ServerError> {
        let path = format!(
            "/user/repos?per_page={per}&sort=pushed&affiliation=owner,collaborator,organization_member&page={page}"
        );
        let (j, next) = self.get(&path, token)?;
        let repos = j
            .as_array()
            .ok_or_else(|| gh("unexpected response shape"))?
            .iter()
            .filter_map(repo_info)
            .collect();
        Ok((repos, next))
    }

    /// 30 repos of the 1-based `page`. With a `query`: scans up to 10 pages of 100, keeps the
    /// case-insensitive substring matches, then slices.
    pub fn repos(
        &self,
        token: &GitToken,
        query: Option<&str>,
        page: u32,
    ) -> Result<(Vec<RepoInfo>, bool), ServerError> {
        let page = page.max(1);
        let q = query.map(str::trim).filter(|q| !q.is_empty());
        let Some(q) = q else {
            return self.page(token, PAGE, page);
        };
        let q = q.to_lowercase();
        let mut hits = Vec::new();
        for p in 1..=SCAN_PAGES {
            let (repos, next) = self.page(token, 100, p)?;
            hits.extend(
                repos
                    .into_iter()
                    .filter(|r| r.full_name.as_str().to_lowercase().contains(&q)),
            );
            if !next {
                break;
            }
        }
        let start = (page as usize - 1).saturating_mul(PAGE);
        let end = start.saturating_add(PAGE);
        let has_more = hits.len() > end;
        Ok((hits.into_iter().skip(start).take(PAGE).collect(), has_more))
    }

    /// `(branch names, default branch)`.
    pub fn branches(
        &self,
        token: &GitToken,
        repo: &RepoRef,
    ) -> Result<(Vec<String>, String), ServerError> {
        let base = format!("/repos/{}/{}", repo.owner(), repo.name());
        let (info, _) = self.get(&base, token)?;
        let default = info["default_branch"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let (j, _) = self.get(&format!("{base}/branches?per_page=100"), token)?;
        let names = j
            .as_array()
            .ok_or_else(|| gh("unexpected response shape"))?
            .iter()
            .filter_map(|b| b["name"].as_str())
            .filter(|n| BranchName::new(n).is_ok())
            .map(str::to_owned)
            .collect();
        Ok((names, default))
    }
}

/// Issue lists hold at most this many; bodies are cut to `BODY_MAX` characters.
const ISSUES_MAX: usize = 100;
const BODY_MAX: usize = 8000;

impl GitHub {
    fn issue_list(
        &self,
        path: &str,
        token: &GitToken,
        tagged: bool,
    ) -> Result<Vec<Issue>, ServerError> {
        let (j, _) = self.get(path, token)?;
        Ok(j.as_array()
            .ok_or_else(|| gh("unexpected response shape"))?
            .iter()
            .filter_map(|v| issue_of(v, tagged))
            .take(ISSUES_MAX)
            .collect())
    }

    /// Open issues in repositories you own or belong to (PRs dropped), newest activity first.
    /// `label`: only issues that carry it.
    pub fn my_issues(
        &self,
        token: &GitToken,
        label: Option<&str>,
    ) -> Result<Vec<Issue>, ServerError> {
        let label = label.map_or(String::new(), |l| format!("&labels={l}"));
        self.issue_list(
            &format!("/user/issues?filter=repos&state=open&sort=updated&per_page=100{label}"),
            token,
            false,
        )
    }

    /// Open issues of one repository (PRs dropped).
    pub fn repo_issues(
        &self,
        token: &GitToken,
        repo: &RepoRef,
        tagged: bool,
    ) -> Result<Vec<Issue>, ServerError> {
        self.issue_list(
            &format!(
                "/repos/{}/issues?state=open&sort=updated&per_page=50",
                repo.as_str()
            ),
            token,
            tagged,
        )
    }

    pub fn issue(
        &self,
        token: &GitToken,
        r: &IssueRef,
        tagged: bool,
    ) -> Result<Issue, ServerError> {
        let (j, _) = self.get(
            &format!("/repos/{}/issues/{}", r.repo.as_str(), r.number),
            token,
        )?;
        issue_of(&j, tagged)
            .ok_or_else(|| gh("not an issue (a pull request, or an unreadable answer)"))
    }

    /// Who last added `label` to the issue (first 100 events only).
    // ponytail: one page of events; an issue with more than 100 events reads as "not labeled".
    pub fn labeled_by(
        &self,
        token: &GitToken,
        r: &IssueRef,
        label: &str,
    ) -> Result<Option<String>, ServerError> {
        let (j, _) = self.get(
            &format!(
                "/repos/{}/issues/{}/events?per_page=100",
                r.repo.as_str(),
                r.number
            ),
            token,
        )?;
        Ok(j.as_array()
            .into_iter()
            .flatten()
            .rev()
            .filter(|e| {
                e["event"] == "labeled"
                    && e["label"]["name"]
                        .as_str()
                        .is_some_and(|n| n.eq_ignore_ascii_case(label))
            })
            .filter_map(|e| e["actor"]["login"].as_str().map(str::to_owned))
            .next())
    }

    /// The number of a PR (any state) opened from `branch` of the repository itself.
    pub fn pr_for_branch(
        &self,
        token: &GitToken,
        repo: &RepoRef,
        branch: &str,
    ) -> Result<Option<u64>, ServerError> {
        if BranchName::new(branch).is_err() {
            return Ok(None);
        }
        let (j, _) = self.get(
            &format!(
                "/repos/{}/pulls?state=all&per_page=1&head={}:{branch}",
                repo.as_str(),
                repo.owner()
            ),
            token,
        )?;
        Ok(j.as_array()
            .and_then(|a| a.first())
            .and_then(|p| p["number"].as_u64()))
    }

    pub fn comment(&self, token: &GitToken, r: &IssueRef, body: &str) -> Result<(), ServerError> {
        self.request(
            &format!("/repos/{}/issues/{}/comments", r.repo.as_str(), r.number),
            token,
            Some(&serde_json::json!({ "body": body })),
        )
        .map(|_| ())
    }
}

fn issue_of(v: &serde_json::Value, tagged: bool) -> Option<Issue> {
    if v.get("pull_request").is_some() {
        return None;
    }
    let repo = v["repository_url"].as_str()?.rsplit_once("/repos/")?.1;
    let issue = IssueRef {
        repo: RepoRef::new(repo).ok()?,
        number: v["number"].as_u64().filter(|n| *n > 0)?,
    };
    let url = v["html_url"]
        .as_str()
        .filter(|u| u.starts_with("https://") && u.len() < 500)?;
    let updated_ms = v["updated_at"]
        .as_str()
        .and_then(|s| OffsetDateTime::parse(s, &Rfc3339).ok())
        .map_or(0, |t| (t.unix_timestamp_nanos() / 1_000_000).max(0) as u64);
    Some(Issue {
        issue,
        title: clean_text(v["title"].as_str().unwrap_or_default(), 300, false),
        body: clean_text(v["body"].as_str().unwrap_or_default(), BODY_MAX, true),
        author: clean_text(v["user"]["login"].as_str().unwrap_or("ghost"), 100, false),
        trusted: matches!(
            v["author_association"].as_str(),
            Some("OWNER" | "MEMBER" | "COLLABORATOR")
        ),
        labels: v["labels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|l| l["name"].as_str())
            .map(|l| clean_text(l, 50, false))
            .take(20)
            .collect(),
        comments: v["comments"].as_u64().unwrap_or(0).min(u32::MAX as u64) as u32,
        updated_ms,
        url: url.to_owned(),
        tagged,
    })
}

fn repo_info(v: &serde_json::Value) -> Option<RepoInfo> {
    let pushed_at_ms = v["pushed_at"]
        .as_str()
        .and_then(|s| OffsetDateTime::parse(s, &Rfc3339).ok())
        .map_or(0, |t| (t.unix_timestamp_nanos() / 1_000_000).max(0) as u64);
    Some(RepoInfo {
        full_name: RepoRef::new(v["full_name"].as_str()?).ok()?,
        private: v["private"].as_bool().unwrap_or(false),
        default_branch: v["default_branch"].as_str().unwrap_or("main").to_owned(),
        pushed_at_ms,
        description: v["description"]
            .as_str()
            .map(|d| d.chars().filter(|c| !c.is_control()).take(300).collect()),
    })
}
