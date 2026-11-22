use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

use crate::arena::{
    CodingLeaderboard, CodingLeaderboardQuery, CodingRunInput, DecideTrialInput, Leaderboard,
    LeaderboardQuery, OverrideCandidateInput, OverrideCandidateResponse, OverrideCodingRunInput,
    OverrideCodingRunResponse, PanelSelection, RecordRunInput, RecordedCodingRunOutput,
    RecordedRunOutput, ReplaceRosterRequest, RosterListResponse, RosterSlotInput, SelectPanelInput,
    StartTrialInput, Trial, TrialsListResponse,
};
use crate::auth::{CreatedApiToken, User};
use crate::routes::{CreateTokenRequest, ErrorBody, LoginRequest};

pub const APP_SLUG: &str = "model-arena-rust";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiClientError {
    Status { status: u16, message: String },
    Transport(String),
}

impl ApiClientError {
    pub fn is_transient_spoolable(&self) -> bool {
        match self {
            Self::Transport(_) => true,
            Self::Status { status, .. } => *status >= 500 || *status == 429 || *status == 408,
        }
    }

    pub fn is_permanent_client_rejection(&self) -> bool {
        match self {
            Self::Status { status, .. } => {
                if *status == 401 || *status == 403 || *status == 408 || *status == 429 {
                    return false;
                }
                (400..500).contains(status)
            }
            Self::Transport(_) => false,
        }
    }
}

impl std::fmt::Display for ApiClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Status { status, message } => write!(f, "HTTP {status}: {message}"),
            Self::Transport(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for ApiClientError {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientConfig {
    pub app_url: String,
    pub token: String,
}

impl ClientConfig {
    pub fn default_path() -> Result<PathBuf, String> {
        let home = env::var("HOME")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| "HOME environment variable is not set".to_string())?;
        Ok(PathBuf::from(home)
            .join(".config")
            .join(format!("{APP_SLUG}.json")))
    }

    pub fn load(config_path: Option<&Path>) -> Result<Self, String> {
        let env_url = env::var("MCP_APP_URL")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        let env_token = env::var("MCP_APP_TOKEN")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());

        let file_cfg = if env_url.is_some() && env_token.is_some() {
            None
        } else {
            let path = match config_path {
                Some(p) => p.to_path_buf(),
                None => Self::default_path()?,
            };
            if path.exists() {
                let raw = fs::read_to_string(&path)
                    .map_err(|err| format!("failed to read {}: {err}", path.display()))?;
                let parsed: ClientConfigFile = serde_json::from_str(&raw)
                    .map_err(|err| format!("invalid config in {}: {err}", path.display()))?;
                Some(parsed)
            } else {
                None
            }
        };

        let app_url = env_url
            .or_else(|| file_cfg.as_ref().and_then(|c| c.app_url.clone()))
            .map(|v| v.trim().trim_end_matches('/').to_string())
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                "missing appUrl (set MCP_APP_URL or run `make mcp-token` to write ~/.config/model-arena-rust.json)".to_string()
            })?;

        validate_base_url(&app_url)?;

        let token = env_token
            .or_else(|| file_cfg.and_then(|c| c.token))
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                "missing token (set MCP_APP_TOKEN or run `make mcp-token` to write ~/.config/model-arena-rust.json)".to_string()
            })?;

        Ok(Self { app_url, token })
    }

    pub fn write_atomic(&self, path: &Path) -> Result<(), String> {
        validate_base_url(&self.app_url)?;
        let parent = path
            .parent()
            .ok_or_else(|| format!("invalid config path {}", path.display()))?;
        fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create {}: {err}", parent.display()))?;

        let tmp_path = parent.join(format!(
            ".{}.tmp.{}",
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("model-arena-rust.json"),
            Uuid::new_v4()
        ));

        let mut body = serde_json::to_string_pretty(self)
            .map_err(|err| format!("failed to serialize config: {err}"))?;
        body.push('\n');

        write_mode_0600(&tmp_path, body.as_bytes())?;
        fs::rename(&tmp_path, path).map_err(|err| {
            let _ = fs::remove_file(&tmp_path);
            format!("failed to move config into {}: {err}", path.display())
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClientConfigFile {
    app_url: Option<String>,
    token: Option<String>,
}

fn validate_base_url(raw: &str) -> Result<Url, String> {
    let parsed = Url::parse(raw).map_err(|err| format!("invalid appUrl '{raw}': {err}"))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => {
            return Err(format!(
                "appUrl must use http:// or https:// (got '{other}://')"
            ));
        }
    }
    if parsed.query().is_some()
        || parsed.fragment().is_some()
        || raw.contains('?')
        || raw.contains('#')
    {
        return Err("appUrl must not include a query string or fragment".to_string());
    }
    Ok(parsed)
}

#[cfg(unix)]
fn write_mode_0600(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|err| format!("failed to create {}: {err}", path.display()))?;
    file.write_all(bytes)
        .map_err(|err| format!("failed to write {}: {err}", path.display()))?;
    file.sync_all()
        .map_err(|err| format!("failed to sync {}: {err}", path.display()))
}

#[cfg(not(unix))]
fn write_mode_0600(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|err| format!("failed to create {}: {err}", path.display()))?;
    file.write_all(bytes)
        .map_err(|err| format!("failed to write {}: {err}", path.display()))?;
    file.sync_all()
        .map_err(|err| format!("failed to sync {}: {err}", path.display()))
}

#[derive(Clone)]
pub struct ApiClient {
    base_url: String,
    http: reqwest::Client,
}

impl ApiClient {
    pub fn new(cfg: &ClientConfig) -> Result<Self, String> {
        validate_base_url(&cfg.app_url)?;
        let mut headers = HeaderMap::new();
        let bearer = HeaderValue::from_str(&format!("Bearer {}", cfg.token))
            .map_err(|_| "token contains invalid HTTP header characters".to_string())?;
        headers.insert(AUTHORIZATION, bearer);

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|err| format!("failed to build HTTP client: {err}"))?;

        Ok(Self {
            base_url: cfg.app_url.trim_end_matches('/').to_string(),
            http,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    async fn send_json<T: DeserializeOwned>(
        &self,
        req: reqwest::RequestBuilder,
        context: &str,
    ) -> Result<T, ApiClientError> {
        let resp = req.send().await.map_err(|err| {
            ApiClientError::Transport(format!("request to {context} failed: {err}"))
        })?;
        if !resp.status().is_success() {
            return Err(read_api_client_error(resp).await);
        }
        resp.json::<T>()
            .await
            .map_err(|err| ApiClientError::Transport(format!("invalid {context} response: {err}")))
    }

    pub async fn me_typed(&self, timeout: Option<Duration>) -> Result<User, ApiClientError> {
        let mut req = self.http.get(format!("{}/api/auth/me", self.base_url));
        if let Some(t) = timeout {
            req = req.timeout(t);
        }
        self.send_json(req, "/api/auth/me").await
    }

    pub async fn me(&self) -> Result<User, String> {
        self.me_typed(None).await.map_err(|e| e.to_string())
    }

    pub async fn list_users(&self) -> Result<Vec<User>, String> {
        let req = self.http.get(format!("{}/api/users", self.base_url));
        self.send_json(req, "/api/users")
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn select_panel(
        &self,
        input: &SelectPanelInput,
        timeout: Option<Duration>,
    ) -> Result<PanelSelection, ApiClientError> {
        let mut req = self
            .http
            .post(format!("{}/api/panel/select", self.base_url))
            .json(input);
        if let Some(t) = timeout {
            req = req.timeout(t);
        }
        self.send_json(req, "/api/panel/select").await
    }

    pub async fn record_run(
        &self,
        input: &RecordRunInput,
    ) -> Result<RecordedRunOutput, ApiClientError> {
        let req = self
            .http
            .post(format!("{}/api/runs", self.base_url))
            .json(input);
        self.send_json(req, "/api/runs").await
    }

    pub async fn get_leaderboard(
        &self,
        query: &LeaderboardQuery,
    ) -> Result<Leaderboard, ApiClientError> {
        let mut url = Url::parse(&format!("{}/api/leaderboard", self.base_url))
            .map_err(|err| ApiClientError::Transport(err.to_string()))?;
        {
            let mut pairs = url.query_pairs_mut();
            if let Some(days) = query.days {
                pairs.append_pair("days", &days.to_string());
            }
            if let Some(skill) = query
                .skill
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                pairs.append_pair("skill", skill);
            }
            if let Some(repo) = query
                .repo
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                pairs.append_pair("repo", repo);
            }
            if let Some(from) = query.from {
                pairs.append_pair("from", &from.to_rfc3339());
            }
            if let Some(to) = query.to {
                pairs.append_pair("to", &to.to_rfc3339());
            }
        }
        let req = self.http.get(url);
        self.send_json(req, "/api/leaderboard").await
    }

    pub async fn list_roster(
        &self,
        skill: Option<&str>,
    ) -> Result<RosterListResponse, ApiClientError> {
        let mut url = Url::parse(&format!("{}/api/roster", self.base_url))
            .map_err(|err| ApiClientError::Transport(err.to_string()))?;
        if let Some(s) = skill.map(str::trim).filter(|s| !s.is_empty()) {
            url.query_pairs_mut().append_pair("skill", s);
        }
        let req = self.http.get(url);
        self.send_json(req, "/api/roster").await
    }

    pub async fn set_roster(
        &self,
        skill: &str,
        slots: Vec<RosterSlotInput>,
    ) -> Result<RosterListResponse, ApiClientError> {
        let trimmed = skill.trim();
        if trimmed.is_empty() {
            return Err(ApiClientError::Status {
                status: 400,
                message: "skill is required".to_string(),
            });
        }
        let mut url = Url::parse(&format!("{}/api/roster", self.base_url))
            .map_err(|err| ApiClientError::Transport(err.to_string()))?;
        url.path_segments_mut()
            .map_err(|_| ApiClientError::Transport("invalid base URL path".to_string()))?
            .push(trimmed);
        let req = self.http.put(url).json(&ReplaceRosterRequest { slots });
        self.send_json(req, "/api/roster/{skill}").await
    }

    pub async fn list_trials(
        &self,
        skill: Option<&str>,
        status: Option<&str>,
    ) -> Result<TrialsListResponse, ApiClientError> {
        let mut url = Url::parse(&format!("{}/api/trials", self.base_url))
            .map_err(|err| ApiClientError::Transport(err.to_string()))?;
        {
            let mut pairs = url.query_pairs_mut();
            if let Some(s) = skill.map(str::trim).filter(|s| !s.is_empty()) {
                pairs.append_pair("skill", s);
            }
            if let Some(st) = status.map(str::trim).filter(|s| !s.is_empty()) {
                pairs.append_pair("status", st);
            }
        }
        let req = self.http.get(url);
        self.send_json(req, "/api/trials").await
    }

    pub async fn start_trial(&self, input: &StartTrialInput) -> Result<Trial, ApiClientError> {
        let req = self
            .http
            .post(format!("{}/api/trials", self.base_url))
            .json(input);
        self.send_json(req, "/api/trials").await
    }

    pub async fn decide_trial(
        &self,
        trial_id: i64,
        decision: &str,
    ) -> Result<Trial, ApiClientError> {
        let req = self
            .http
            .post(format!("{}/api/trials/{trial_id}/decide", self.base_url))
            .json(&DecideTrialInput {
                decision: decision.to_string(),
            });
        self.send_json(req, "/api/trials/{id}/decide").await
    }

    pub async fn override_candidate(
        &self,
        candidate_id: i64,
        input: &OverrideCandidateInput,
    ) -> Result<OverrideCandidateResponse, ApiClientError> {
        let req = self
            .http
            .post(format!(
                "{}/api/candidates/{candidate_id}/override",
                self.base_url
            ))
            .json(input);
        self.send_json(req, "/api/candidates/{id}/override").await
    }

    pub async fn record_coding_run(
        &self,
        input: &CodingRunInput,
    ) -> Result<RecordedCodingRunOutput, ApiClientError> {
        let req = self
            .http
            .post(format!("{}/api/coding-runs", self.base_url))
            .json(input);
        self.send_json(req, "/api/coding-runs").await
    }

    pub async fn override_coding_run(
        &self,
        coding_run_id: i64,
        input: &OverrideCodingRunInput,
    ) -> Result<OverrideCodingRunResponse, ApiClientError> {
        let req = self
            .http
            .post(format!(
                "{}/api/coding-runs/{coding_run_id}/override",
                self.base_url
            ))
            .json(input);
        self.send_json(req, "/api/coding-runs/{id}/override").await
    }

    pub async fn get_coding_leaderboard(
        &self,
        query: &CodingLeaderboardQuery,
    ) -> Result<CodingLeaderboard, ApiClientError> {
        let mut url = Url::parse(&format!("{}/api/coding-leaderboard", self.base_url))
            .map_err(|err| ApiClientError::Transport(err.to_string()))?;
        {
            let mut pairs = url.query_pairs_mut();
            if let Some(days) = query.days {
                pairs.append_pair("days", &days.to_string());
            }
            if let Some(repo) = query
                .repo
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                pairs.append_pair("repo", repo);
            }
            if let Some(tk) = query
                .task_kind
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                pairs.append_pair("taskKind", tk);
            }
            if let Some(sb) = query
                .size_bucket
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                pairs.append_pair("sizeBucket", sb);
            }
            if let Some(from) = query.from {
                pairs.append_pair("from", &from.to_rfc3339());
            }
            if let Some(to) = query.to {
                pairs.append_pair("to", &to.to_rfc3339());
            }
        }
        let req = self.http.get(url);
        self.send_json(req, "/api/coding-leaderboard").await
    }
}

pub async fn mint_token_via_login(
    app_url: &str,
    email: &str,
    password: &str,
    token_name: &str,
) -> Result<CreatedApiToken, String> {
    let base = app_url.trim().trim_end_matches('/');
    validate_base_url(base)?;

    let client = reqwest::Client::builder()
        .cookie_store(true)
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|err| format!("failed to build HTTP client: {err}"))?;

    let login_resp = client
        .post(format!("{base}/api/auth/login"))
        .json(&LoginRequest {
            email: email.to_string(),
            password: password.to_string(),
        })
        .send()
        .await
        .map_err(|err| format!("login request failed: {err}"))?;

    if !login_resp.status().is_success() {
        return Err(read_api_error(login_resp).await);
    }

    let token_resp = client
        .post(format!("{base}/api/tokens"))
        .json(&CreateTokenRequest {
            name: token_name.to_string(),
        })
        .send()
        .await
        .map_err(|err| format!("token creation request failed: {err}"))?;

    if !token_resp.status().is_success() {
        return Err(read_api_error(token_resp).await);
    }

    token_resp
        .json::<CreatedApiToken>()
        .await
        .map_err(|err| format!("invalid /api/tokens response: {err}"))
}

async fn read_api_client_error(resp: reqwest::Response) -> ApiClientError {
    let status = resp.status().as_u16();
    if let Ok(err_body) = resp.json::<ErrorBody>().await {
        return ApiClientError::Status {
            status,
            message: err_body.error,
        };
    }
    ApiClientError::Status {
        status,
        message: format!("HTTP {status}"),
    }
}

async fn read_api_error(resp: reqwest::Response) -> String {
    read_api_client_error(resp).await.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_fields_in_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("model-arena-rust.json");
        fs::write(
            &path,
            r#"{"appUrl":"http://localhost:8080","token":"pat_x_y","typo":true}"#,
        )
        .unwrap();

        let err = ClientConfig::load(Some(&path)).unwrap_err();
        assert!(err.contains("unknown field `typo`"), "got: {err}");
    }

    #[test]
    fn writes_atomic_0600_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("model-arena-rust.json");
        let cfg = ClientConfig {
            app_url: "http://127.0.0.1:8080".to_string(),
            token: "pat_00000000-0000-0000-0000-000000000000_secret".to_string(),
        };
        cfg.write_atomic(&path).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        // Overwriting an existing config file atomically preserves 0600 mode,
        // while `write_mode_0600` itself refuses pre-existing temp paths/symlinks.
        let updated = ClientConfig {
            app_url: "http://127.0.0.1:8080".to_string(),
            token: "pat_00000000-0000-0000-0000-000000000000_rotated".to_string(),
        };
        updated.write_atomic(&path).unwrap();
        assert_eq!(ClientConfig::load(Some(&path)).unwrap(), updated);

        let existing = dir.path().join("preexisting.tmp");
        fs::write(&existing, b"stale").unwrap();
        assert!(write_mode_0600(&existing, b"secret").is_err());
    }

    #[test]
    fn rejects_app_urls_with_queries_or_fragments() {
        for bad in [
            "http://localhost:8080/#focus",
            "http://localhost:8080?view=focus",
            "https://model-arena-rust.home/?",
            "https://model-arena-rust.home/#",
        ] {
            let cfg = ClientConfig {
                app_url: bad.to_string(),
                token: "pat_00000000-0000-0000-0000-000000000000_secret".to_string(),
            };
            assert!(ApiClient::new(&cfg).is_err(), "expected error for {bad}");
        }
    }
}
