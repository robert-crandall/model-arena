use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::apiclient::{ApiClient, ApiClientError, ClientConfig};
use crate::arena::service::{
    validate_and_normalize_coding_run_input, validate_and_normalize_record_run_input,
};
use crate::arena::shadow::{
    RecordedShadowBoutOutput, ShadowBoutInput, validate_and_normalize_shadow_bout_input,
};
use crate::arena::types::{
    CodingRunInput, RecordRunInput, RecordedCodingRunOutput, RecordedRunOutput, Trial,
};

const STALE_CLAIM_AGE: Duration = Duration::from_secs(120);

static SPOOL_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct SpoolTokenRecord {
    app_url: String,
    token_fallback: String,
    user_id: Uuid,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FlushSummary {
    pub flushed: usize,
    pub pending: usize,
    pub rejected: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub completed_trials: Vec<Trial>,
}

pub struct SpoolScope {
    base_dir_override: Option<PathBuf>,
    app_url: String,
    token_fallback: String,
    is_anon: bool,
    verified_scope: Mutex<Option<String>>,
}

impl SpoolScope {
    pub fn from_config(cfg: &ClientConfig) -> Self {
        Self::with_base_dir(cfg, None)
    }

    pub fn with_base_dir(cfg: &ClientConfig, base_dir_override: Option<PathBuf>) -> Self {
        let app_url = cfg
            .app_url
            .trim()
            .trim_end_matches('/')
            .to_ascii_lowercase();
        let token = cfg.token.trim();
        let is_anon = token.is_empty();
        let token_acct = if is_anon {
            "anon".to_string()
        } else {
            let mut hasher = Sha256::new();
            hasher.update(token.as_bytes());
            let digest = hasher.finalize();
            format!("tok_{}", hex::encode(&digest[..8]))
        };

        let mut fb_hasher = Sha256::new();
        fb_hasher.update(format!("{app_url}|{token_acct}").as_bytes());
        let fb_digest = fb_hasher.finalize();
        let token_fallback = hex::encode(&fb_digest[..8]);

        let scope = Self {
            base_dir_override,
            app_url,
            token_fallback,
            is_anon,
            verified_scope: Mutex::new(None),
        };

        if !scope.is_anon
            && let Some(uid) = scope.lookup_cached_user_id()
        {
            let verified = user_scope_hash(&scope.app_url, uid);
            if let Ok(mut guard) = scope.verified_scope.lock() {
                *guard = Some(verified.clone());
            }
            scope.migrate_all_user_fallbacks(uid, &verified);
        }

        scope
    }

    pub fn spool_base_dir(&self) -> Result<PathBuf, String> {
        if let Some(custom) = &self.base_dir_override {
            return Ok(custom.clone());
        }
        if let Ok(env_dir) = env::var("MODEL_ARENA_SPOOL_DIR") {
            let trimmed = env_dir.trim();
            if !trimmed.is_empty() {
                return Ok(PathBuf::from(trimmed));
            }
        }
        if let Ok(xdg) = env::var("XDG_CACHE_HOME") {
            let trimmed = xdg.trim();
            if !trimmed.is_empty() {
                return Ok(PathBuf::from(trimmed).join("model-arena").join("spool"));
            }
        }
        let home = env::var("HOME")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| "HOME environment variable is not set".to_string())?;
        Ok(PathBuf::from(home)
            .join(".cache")
            .join("model-arena")
            .join("spool"))
    }

    fn tokens_dir(&self) -> Result<PathBuf, String> {
        Ok(self.spool_base_dir()?.join("tokens"))
    }

    fn token_mapping_filename(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(format!("{}|{}", self.app_url, self.token_fallback).as_bytes());
        let digest = hasher.finalize();
        format!("{}.json", hex::encode(&digest[..16]))
    }

    fn lookup_cached_user_id(&self) -> Option<Uuid> {
        if self.token_fallback.is_empty() {
            return None;
        }
        let dir = self.tokens_dir().ok()?;
        let raw = fs::read_to_string(dir.join(self.token_mapping_filename())).ok()?;
        let rec: SpoolTokenRecord = serde_json::from_str(&raw).ok()?;
        Some(rec.user_id)
    }

    fn write_token_mapping_file(&self, user_id: Uuid) {
        if self.token_fallback.is_empty() {
            return;
        }
        let Ok(dir) = self.tokens_dir() else {
            return;
        };
        if create_dir_0700(&dir).is_err() {
            return;
        }
        let rec = SpoolTokenRecord {
            app_url: self.app_url.clone(),
            token_fallback: self.token_fallback.clone(),
            user_id,
        };
        let Ok(raw) = serde_json::to_vec(&rec) else {
            return;
        };
        let tmp_path = dir.join(format!(
            ".tok-{}-{}.tmp",
            std::process::id(),
            SPOOL_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        if write_new_0600(&tmp_path, &raw).is_err() {
            let _ = fs::remove_file(&tmp_path);
            return;
        }
        let target = dir.join(self.token_mapping_filename());
        if fs::rename(&tmp_path, &target).is_err() {
            let _ = fs::remove_file(&tmp_path);
        }
    }

    pub fn active_scope(&self) -> String {
        if let Ok(guard) = self.verified_scope.lock()
            && let Some(verified) = guard.as_ref()
        {
            return verified.clone();
        }
        if !self.is_anon
            && let Some(uid) = self.lookup_cached_user_id()
        {
            let verified = user_scope_hash(&self.app_url, uid);
            if let Ok(mut guard) = self.verified_scope.lock() {
                *guard = Some(verified.clone());
            }
            self.migrate_all_user_fallbacks(uid, &verified);
            return verified;
        }
        self.token_fallback.clone()
    }

    pub fn active_spool_dir(&self) -> Result<PathBuf, String> {
        let active = self.active_scope();
        if active.trim().is_empty() {
            return Err("spool scope is required".to_string());
        }
        Ok(self.spool_base_dir()?.join(active))
    }

    pub fn record_verified_user(&self, user_id: Uuid) {
        let verified = user_scope_hash(&self.app_url, user_id);
        if let Ok(mut guard) = self.verified_scope.lock() {
            *guard = Some(verified.clone());
        }
        if !self.is_anon {
            self.write_token_mapping_file(user_id);
            self.migrate_all_user_fallbacks(user_id, &verified);
        }
    }

    pub async fn ensure_verified(&self, client: &ApiClient) {
        let already = self
            .verified_scope
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .is_some();
        if already {
            return;
        }
        if let Ok(user) = client.me_typed(Some(Duration::from_millis(1200))).await {
            self.record_verified_user(user.id);
        }
    }

    pub fn reconcile_verified_fallbacks(&self) {
        if self.is_anon {
            return;
        }
        let verified = self
            .verified_scope
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .unwrap_or_default();
        if verified.is_empty() {
            return;
        }
        if !self.token_fallback.is_empty() && self.token_fallback != verified {
            self.migrate_fallback_dir(&self.token_fallback, &verified);
        }
        if let Ok(dir) = self.tokens_dir()
            && let Ok(entries) = fs::read_dir(&dir)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                let Ok(raw) = fs::read_to_string(&path) else {
                    continue;
                };
                if let Ok(rec) = serde_json::from_str::<SpoolTokenRecord>(&raw)
                    && rec.app_url == self.app_url
                    && user_scope_hash(&self.app_url, rec.user_id) == verified
                    && !rec.token_fallback.is_empty()
                    && rec.token_fallback != verified
                {
                    self.migrate_fallback_dir(&rec.token_fallback, &verified);
                }
            }
        }
    }

    fn migrate_all_user_fallbacks(&self, user_id: Uuid, verified: &str) {
        if !self.token_fallback.is_empty() && self.token_fallback != verified {
            self.migrate_fallback_dir(&self.token_fallback, verified);
        }
        if let Ok(dir) = self.tokens_dir()
            && let Ok(entries) = fs::read_dir(&dir)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                let Ok(raw) = fs::read_to_string(&path) else {
                    continue;
                };
                if let Ok(rec) = serde_json::from_str::<SpoolTokenRecord>(&raw)
                    && rec.user_id == user_id
                    && rec.app_url == self.app_url
                    && !rec.token_fallback.is_empty()
                    && rec.token_fallback != verified
                {
                    self.migrate_fallback_dir(&rec.token_fallback, verified);
                }
            }
        }
    }

    fn migrate_fallback_dir(&self, from_scope: &str, to_scope: &str) {
        if from_scope.is_empty() || to_scope.is_empty() || from_scope == to_scope {
            return;
        }
        let Ok(base) = self.spool_base_dir() else {
            return;
        };
        let src_dir = base.join(from_scope);
        if !src_dir.is_dir() {
            return;
        }
        recover_stale_claims(&src_dir);
        let Ok(entries) = fs::read_dir(&src_dir) else {
            return;
        };
        let dst_dir = base.join(to_scope);
        if create_dir_0700(&dst_dir).is_err() {
            return;
        }
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(".json") && !name.ends_with(".bout") {
                continue;
            }
            let src_path = src_dir.join(&name);
            let mut dst_path = dst_dir.join(&name);
            if dst_path.exists() {
                let (stem, ext) = if let Some(s) = name.strip_suffix(".coding.json") {
                    (s, "coding.json")
                } else if let Some(s) = name.strip_suffix(".bout") {
                    (s, "bout")
                } else {
                    (name.trim_end_matches(".json"), "json")
                };
                dst_path = dst_dir.join(format!(
                    "{stem}.{}.{}.{ext}",
                    std::process::id(),
                    SPOOL_SEQ.fetch_add(1, Ordering::Relaxed)
                ));
            }
            let _ = fs::rename(&src_path, &dst_path);
        }
        sync_dir(&dst_dir);
        let _ = fs::remove_dir(&src_dir);
    }
}

fn user_scope_hash(app_url: &str, user_id: Uuid) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("{app_url}|uid:{user_id}").as_bytes());
    let digest = hasher.finalize();
    hex::encode(&digest[..8])
}

#[cfg(unix)]
fn sync_dir(dir: &Path) {
    if let Ok(f) = fs::File::open(dir) {
        let _ = f.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) {}

#[cfg(unix)]
fn create_dir_0700(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|err| format!("failed to create {}: {err}", dir.display()))?;
    if let Some(parent) = dir.parent() {
        sync_dir(parent);
    }
    Ok(())
}

#[cfg(not(unix))]
fn create_dir_0700(dir: &Path) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|err| format!("failed to create {}: {err}", dir.display()))
}

#[cfg(unix)]
fn write_new_0600(path: &Path, bytes: &[u8]) -> Result<(), String> {
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
fn write_new_0600(path: &Path, bytes: &[u8]) -> Result<(), String> {
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

fn spool_filename(run_key: &str, sent_nanos: i64, suffix: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(run_key.as_bytes());
    let digest = hasher.finalize();
    let seq = SPOOL_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    format!(
        "{}.{:020}.{}.{seq}.{suffix}",
        hex::encode(&digest[..16]),
        sent_nanos.max(0),
        std::process::id()
    )
}

pub fn write_spool_run(scope: &SpoolScope, mut input: RecordRunInput) -> Result<(), String> {
    let dir = scope.active_spool_dir()?;
    create_dir_0700(&dir)?;

    let sent_at = input.client_sent_at.unwrap_or_else(Utc::now);
    input.client_sent_at = Some(sent_at);
    input.queue_only = false;

    let sent_nanos = sent_at.timestamp_nanos_opt().unwrap_or(0);
    let target = dir.join(spool_filename(&input.run_key, sent_nanos, "json"));
    let raw = serde_json::to_vec(&input)
        .map_err(|err| format!("failed to serialize spooled run: {err}"))?;

    let tmp_path = dir.join(format!(
        ".spool-{}-{}.tmp",
        std::process::id(),
        SPOOL_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    write_new_0600(&tmp_path, &raw)?;
    fs::rename(&tmp_path, &target).map_err(|err| {
        let _ = fs::remove_file(&tmp_path);
        format!("failed to commit spool file {}: {err}", target.display())
    })?;
    sync_dir(&dir);
    Ok(())
}

pub fn write_spool_coding_run(scope: &SpoolScope, mut input: CodingRunInput) -> Result<(), String> {
    let dir = scope.active_spool_dir()?;
    create_dir_0700(&dir)?;

    let sent_at = input.client_sent_at.unwrap_or_else(Utc::now);
    input.client_sent_at = Some(sent_at);
    input.queue_only = false;

    let sent_nanos = sent_at.timestamp_nanos_opt().unwrap_or(0);
    let target = dir.join(spool_filename(&input.run_key, sent_nanos, "coding.json"));
    let raw = serde_json::to_vec(&input)
        .map_err(|err| format!("failed to serialize spooled coding run: {err}"))?;

    let tmp_path = dir.join(format!(
        ".spool-coding-{}-{}.tmp",
        std::process::id(),
        SPOOL_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    write_new_0600(&tmp_path, &raw)?;
    fs::rename(&tmp_path, &target).map_err(|err| {
        let _ = fs::remove_file(&tmp_path);
        format!("failed to commit spool file {}: {err}", target.display())
    })?;
    sync_dir(&dir);
    Ok(())
}

/// Spools shadow bouts with a `.bout` suffix (non-`.json`) so older running MCP binaries
/// scanning `*.json` never treat them as unrecognized runs and quarantine them as `.corrupt`.
pub fn write_spool_shadow_bout(
    scope: &SpoolScope,
    mut input: ShadowBoutInput,
) -> Result<(), String> {
    let dir = scope.active_spool_dir()?;
    create_dir_0700(&dir)?;

    let sent_at = input.client_sent_at.unwrap_or_else(Utc::now);
    input.client_sent_at = Some(sent_at);
    input.queue_only = false;

    let sent_nanos = sent_at.timestamp_nanos_opt().unwrap_or(0);
    let target = dir.join(spool_filename(&input.bout_key, sent_nanos, "bout"));
    let raw = serde_json::to_vec(&input)
        .map_err(|err| format!("failed to serialize spooled shadow bout: {err}"))?;

    let tmp_path = dir.join(format!(
        ".spool-bout-{}-{}.tmp",
        std::process::id(),
        SPOOL_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    write_new_0600(&tmp_path, &raw)?;
    fs::rename(&tmp_path, &target).map_err(|err| {
        let _ = fs::remove_file(&tmp_path);
        format!("failed to commit spool file {}: {err}", target.display())
    })?;
    sync_dir(&dir);
    Ok(())
}

fn restore_claim_without_overwrite(claim_path: &Path, full_path: &Path) {
    match fs::hard_link(claim_path, full_path) {
        Ok(()) => {
            let _ = fs::remove_file(claim_path);
            return;
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            let _ = fs::remove_file(claim_path);
            return;
        }
        Err(_) => {}
    }
    if !full_path.exists() {
        let _ = fs::rename(claim_path, full_path);
        return;
    }
    let _ = fs::remove_file(claim_path);
}

fn recover_stale_claims(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let now_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as i128;
    let stale_cutoff_nanos = now_nanos - (STALE_CLAIM_AGE.as_nanos() as i128);

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some((base_end, suffix_start)) = name
            .find(".json.sending.")
            .map(|idx| (idx + ".json".len(), idx + ".json.sending.".len()))
            .or_else(|| {
                name.find(".bout.sending.")
                    .map(|idx| (idx + ".bout".len(), idx + ".bout.sending.".len()))
            })
        else {
            continue;
        };
        if base_end <= ".bout".len() {
            continue;
        }
        let suffix = &name[suffix_start..];
        let mut parts = suffix.split('.');
        let _pid = parts.next();
        let claim_nanos = parts
            .next()
            .and_then(|s| s.parse::<i128>().ok())
            .unwrap_or_else(|| {
                entry
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos() as i128)
                    .unwrap_or(0)
            });

        if claim_nanos > stale_cutoff_nanos {
            continue;
        }
        let base_name = &name[..base_end];
        restore_claim_without_overwrite(&dir.join(&name), &dir.join(base_name));
    }
}

pub async fn flush_spool(client: &ApiClient, scope: &SpoolScope) -> FlushSummary {
    scope.ensure_verified(client).await;
    scope.reconcile_verified_fallbacks();
    let Ok(dir) = scope.active_spool_dir() else {
        return FlushSummary::default();
    };
    if !dir.is_dir() {
        return FlushSummary::default();
    }
    recover_stale_claims(&dir);

    let Ok(entries) = fs::read_dir(&dir) else {
        return FlushSummary::default();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if !path.is_file() {
                return None;
            }
            let name = e.file_name().to_string_lossy().to_string();
            if (name.ends_with(".json") || name.ends_with(".bout")) && !name.starts_with('.') {
                Some(name)
            } else {
                None
            }
        })
        .collect();
    names.sort();

    let mut summary = FlushSummary::default();
    for (i, name) in names.iter().enumerate() {
        let full_path = dir.join(name);
        let now_nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let seq = SPOOL_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
        let claim_path = dir.join(format!(
            "{name}.sending.{}.{now_nanos}.{seq}",
            std::process::id()
        ));
        if fs::rename(&full_path, &claim_path).is_err() {
            continue;
        }

        let Ok(raw) = fs::read(&claim_path) else {
            restore_claim_without_overwrite(&claim_path, &full_path);
            continue;
        };

        let is_bout = name.ends_with(".bout");
        let is_coding = name.ends_with(".coding.json");
        if is_bout {
            let Ok(bout) = serde_json::from_slice::<ShadowBoutInput>(&raw) else {
                let _ = fs::rename(&claim_path, dir.join(format!("{name}.corrupt")));
                continue;
            };
            match client.record_shadow_bout(&bout).await {
                Ok(_) => {
                    let _ = fs::remove_file(&claim_path);
                    summary.flushed += 1;
                }
                Err(crate::apiclient::ApiClientError::Status { status: 404, .. }) => {
                    // Older server without /api/shadow-bouts: keep .bout queued rather than rejecting it.
                    restore_claim_without_overwrite(&claim_path, &full_path);
                    summary.pending += 1;
                }
                Err(err) if err.is_permanent_client_rejection() => {
                    let _ = fs::rename(&claim_path, dir.join(format!("{name}.rejected")));
                    summary.rejected += 1;
                }
                Err(_) => {
                    restore_claim_without_overwrite(&claim_path, &full_path);
                    summary.pending += names.len() - i;
                    return summary;
                }
            }
        } else if is_coding {
            let Ok(coding_run) = serde_json::from_slice::<CodingRunInput>(&raw) else {
                let _ = fs::rename(&claim_path, dir.join(format!("{name}.corrupt")));
                continue;
            };
            match client.record_coding_run(&coding_run).await {
                Ok(_) => {
                    let _ = fs::remove_file(&claim_path);
                    summary.flushed += 1;
                }
                Err(err) if err.is_permanent_client_rejection() => {
                    let _ = fs::rename(&claim_path, dir.join(format!("{name}.rejected")));
                    summary.rejected += 1;
                }
                Err(_) => {
                    restore_claim_without_overwrite(&claim_path, &full_path);
                    summary.pending = names.len() - i;
                    return summary;
                }
            }
        } else {
            let Ok(run) = serde_json::from_slice::<RecordRunInput>(&raw) else {
                let _ = fs::rename(&claim_path, dir.join(format!("{name}.corrupt")));
                continue;
            };
            match client.record_run(&run).await {
                Ok(recorded) => {
                    let _ = fs::remove_file(&claim_path);
                    summary.flushed += 1;
                    for ct in recorded.completed_trials {
                        if !summary.completed_trials.iter().any(|e| e.id == ct.id) {
                            summary.completed_trials.push(ct);
                        }
                    }
                }
                Err(err) if err.is_permanent_client_rejection() => {
                    let _ = fs::rename(&claim_path, dir.join(format!("{name}.rejected")));
                    summary.rejected += 1;
                }
                Err(_) => {
                    restore_claim_without_overwrite(&claim_path, &full_path);
                    summary.pending = names.len() - i;
                    return summary;
                }
            }
        }
    }

    summary
}

pub async fn submit_or_spool_run(
    client: &ApiClient,
    scope: &SpoolScope,
    mut input: RecordRunInput,
) -> Result<RecordedRunOutput, String> {
    validate_and_normalize_record_run_input(&mut input).map_err(|e| e.message)?;
    if input.client_sent_at.is_none() {
        input.client_sent_at = Some(Utc::now());
    }

    if input.queue_only {
        write_spool_run(scope, input.clone())
            .map_err(|err| format!("local spool failed: {err}"))?;
        return Ok(RecordedRunOutput {
            run_id: 0,
            run_key: input.run_key,
            completed_trials: Vec::new(),
            spooled: true,
        });
    }

    let _ = tokio::time::timeout(Duration::from_millis(1500), flush_spool(client, scope)).await;

    match client.record_run(&input).await {
        Ok(out) => Ok(out),
        Err(err) if !err.is_transient_spoolable() => Err(err.to_string()),
        Err(err) => {
            write_spool_run(scope, input.clone())
                .map_err(|serr| format!("submit failed ({err}) and local spool failed: {serr}"))?;
            Ok(RecordedRunOutput {
                run_id: 0,
                run_key: input.run_key,
                completed_trials: Vec::new(),
                spooled: true,
            })
        }
    }
}

pub async fn submit_or_spool_coding_run(
    client: &ApiClient,
    scope: &SpoolScope,
    mut input: CodingRunInput,
) -> Result<RecordedCodingRunOutput, String> {
    let size_bucket = validate_and_normalize_coding_run_input(&mut input).map_err(|e| e.message)?;
    if input.client_sent_at.is_none() {
        input.client_sent_at = Some(Utc::now());
    }

    if input.queue_only {
        write_spool_coding_run(scope, input.clone())
            .map_err(|err| format!("local spool failed: {err}"))?;
        return Ok(RecordedCodingRunOutput {
            run_id: 0,
            run_key: input.run_key,
            size_bucket,
            spooled: true,
        });
    }

    let _ = tokio::time::timeout(Duration::from_millis(1500), flush_spool(client, scope)).await;

    match client.record_coding_run(&input).await {
        Ok(out) => Ok(out),
        Err(err) if !err.is_transient_spoolable() => Err(err.to_string()),
        Err(err) => {
            write_spool_coding_run(scope, input.clone())
                .map_err(|serr| format!("submit failed ({err}) and local spool failed: {serr}"))?;
            Ok(RecordedCodingRunOutput {
                run_id: 0,
                run_key: input.run_key,
                size_bucket,
                spooled: true,
            })
        }
    }
}

pub async fn submit_or_spool_shadow_bout(
    client: &ApiClient,
    scope: &SpoolScope,
    mut input: ShadowBoutInput,
) -> Result<RecordedShadowBoutOutput, String> {
    validate_and_normalize_shadow_bout_input(&mut input).map_err(|e| e.message)?;
    if input.client_sent_at.is_none() {
        input.client_sent_at = Some(Utc::now());
    }

    if input.queue_only {
        write_spool_shadow_bout(scope, input.clone())
            .map_err(|err| format!("local spool failed: {err}"))?;
        return Ok(RecordedShadowBoutOutput {
            bout_id: 0,
            bout_key: input.bout_key,
            winner: input.winner,
            spooled: true,
        });
    }

    let _ = tokio::time::timeout(Duration::from_millis(1500), flush_spool(client, scope)).await;

    match client.record_shadow_bout(&input).await {
        Ok(out) => Ok(out),
        Err(err)
            if err.is_transient_spoolable()
                || matches!(err, ApiClientError::Status { status: 404, .. }) =>
        {
            write_spool_shadow_bout(scope, input.clone())
                .map_err(|serr| format!("submit failed ({err}) and local spool failed: {serr}"))?;
            Ok(RecordedShadowBoutOutput {
                bout_id: 0,
                bout_key: input.bout_key,
                winner: input.winner,
                spooled: true,
            })
        }
        Err(err) => Err(err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arena::types::RunLegInput;

    fn sample_run(run_key: &str) -> RecordRunInput {
        RecordRunInput {
            run_key: run_key.to_string(),
            skill: "adversarial-review".to_string(),
            artifact_kind: "pr_diff".to_string(),
            artifact_ref: "owner/repo#1".to_string(),
            repo: "owner/repo".to_string(),
            phase: "independent".to_string(),
            host_model: "gemini-4-argon".to_string(),
            host_reasoning_effort: String::new(),
            host_lineage: "google".to_string(),
            harness: "copilot".to_string(),
            machine: "test-host".to_string(),
            approach_verdict: "keep".to_string(),
            winning_role: String::new(),
            occurred_at: None,
            client_sent_at: None,
            queue_only: true,
            legs: vec![RunLegInput {
                slot_index: 0,
                role: "reviewer".to_string(),
                lineage: "openai".to_string(),
                model: "gpt-6.1-sol".to_string(),
                reasoning_effort: String::new(),
                is_challenger: false,
                trial_id: None,
                status: "ok".to_string(),
                duration_ms: 4200,
                won_core_plan: false,
                cherry_picks_count: 0,
                critic_adopted_count: 0,
                critic_bloat_dropped_count: 0,
            }],
            candidates: Vec::new(),
        }
    }

    #[test]
    fn token_rotation_migrates_spooled_runs_to_verified_user_scope() {
        let dir = tempfile::tempdir().unwrap();
        let cfg1 = ClientConfig {
            app_url: "http://127.0.0.1:18080".to_string(),
            token: "pat_11111111-1111-1111-1111-111111111111_secret1".to_string(),
        };
        let scope1 = SpoolScope::with_base_dir(&cfg1, Some(dir.path().to_path_buf()));

        // Spool offline before user is verified
        write_spool_run(&scope1, sample_run("offline_run_1")).unwrap();
        let fallback_dir1 = scope1.active_spool_dir().unwrap();
        assert!(fallback_dir1.is_dir());

        // Verify user on token 1 -> migrates fallback_dir1 to verified user scope
        let user_id = Uuid::new_v4();
        scope1.record_verified_user(user_id);
        let verified_dir = scope1.active_spool_dir().unwrap();
        assert_ne!(fallback_dir1, verified_dir);
        assert!(!fallback_dir1.exists());
        assert_eq!(fs::read_dir(&verified_dir).unwrap().count(), 1);

        // Rotate to token 2 while offline -> spool run 2, then verify same user_id
        let cfg2 = ClientConfig {
            app_url: "http://127.0.0.1:18080".to_string(),
            token: "pat_22222222-2222-2222-2222-222222222222_secret2".to_string(),
        };
        let scope2 = SpoolScope::with_base_dir(&cfg2, Some(dir.path().to_path_buf()));
        write_spool_run(&scope2, sample_run("offline_run_2")).unwrap();
        scope2.record_verified_user(user_id);
        assert_eq!(scope2.active_spool_dir().unwrap(), verified_dir);
        assert_eq!(fs::read_dir(&verified_dir).unwrap().count(), 2);
    }

    #[test]
    fn recovers_stale_claims_and_preserves_coding_suffix_and_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = ClientConfig {
            app_url: "http://127.0.0.1:18080".to_string(),
            token: "pat_33333333-3333-3333-3333-333333333333_secret3".to_string(),
        };
        let scope = SpoolScope::with_base_dir(&cfg, Some(dir.path().to_path_buf()));

        write_spool_run(&scope, sample_run("perm_check_run")).unwrap();
        let spool_dir = scope.active_spool_dir().unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir_mode = fs::metadata(&spool_dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(dir_mode, 0o700);
            for entry in fs::read_dir(&spool_dir).unwrap().flatten() {
                let file_mode = entry.metadata().unwrap().permissions().mode() & 0o777;
                assert_eq!(file_mode, 0o600);
            }
        }

        // Write a stale .sending. claim (timestamp 0) and verify recovery
        let stale_claim = spool_dir.join("deadbeef.coding.json.sending.99999.0.1");
        fs::write(&stale_claim, b"{}").unwrap();
        recover_stale_claims(&spool_dir);
        assert!(!stale_claim.exists());
        assert!(spool_dir.join("deadbeef.coding.json").exists());

        // Verify collision during fallback migration preserves .coding.json suffix
        let verified_user_id = Uuid::new_v4();
        let verified_scope = user_scope_hash(&cfg.app_url, verified_user_id);
        let verified_dir = scope.spool_base_dir().unwrap().join(&verified_scope);
        create_dir_0700(&verified_dir).unwrap();
        fs::write(verified_dir.join("deadbeef.coding.json"), b"{}").unwrap();

        scope.record_verified_user(verified_user_id);
        let migrated_names: Vec<String> = fs::read_dir(&verified_dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with("deadbeef"))
            .collect();
        assert_eq!(migrated_names.len(), 2);
        assert!(migrated_names.iter().all(|n| n.ends_with(".coding.json")));
    }

    #[test]
    fn shadow_bout_spool_uses_bout_suffix_and_recovers_stale_claims() {
        use crate::arena::shadow::{ShadowBoutMatrixInput, ShadowBoutSideInput};

        let tmp = tempfile::tempdir().unwrap();
        let cfg = ClientConfig {
            app_url: "https://arena.example".to_string(),
            token: "pat_bout_token".to_string(),
        };
        let scope = SpoolScope::with_base_dir(&cfg, Some(tmp.path().to_path_buf()));
        let bout = ShadowBoutInput {
            bout_key: "sb_spool_1".to_string(),
            track: "coding".to_string(),
            champion_source: "headless".to_string(),
            champion_human_turns: 0,
            repo: "robert-crandall/demo".to_string(),
            base_sha: "abc123".to_string(),
            task_ref: "PR #7".to_string(),
            task_kind: "bugfix".to_string(),
            prompt_sha256: "hash".to_string(),
            champion: ShadowBoutSideInput {
                model: "claude-opus-5.5".to_string(),
                status: "ok".to_string(),
                metrics: serde_json::json!({}),
                ..Default::default()
            },
            challenger: ShadowBoutSideInput {
                model: "gpt-6.2-sol".to_string(),
                status: "ok".to_string(),
                metrics: serde_json::json!({}),
                ..Default::default()
            },
            matrix: ShadowBoutMatrixInput::default(),
            winner: String::new(),
            judge_model: String::new(),
            judge_lineage: String::new(),
            judge_rationale: String::new(),
            judged_at: None,
            occurred_at: None,
            client_sent_at: None,
            queue_only: true,
        };
        write_spool_shadow_bout(&scope, bout).unwrap();

        let dir = scope.active_spool_dir().unwrap();
        let names: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names.len(), 1);
        assert!(names[0].ends_with(".bout"));
        assert!(!names[0].ends_with(".json"));

        let original = dir.join(&names[0]);
        let stored: ShadowBoutInput =
            serde_json::from_slice(&fs::read(&original).unwrap()).unwrap();
        assert!(stored.occurred_at.is_none());
        assert!(stored.client_sent_at.is_some());

        // Simulate a stale in-flight claim and verify recovery restores the .bout filename.
        let stale_claim = dir.join(format!("{}.sending.99999.1.1", names[0]));
        fs::rename(&original, &stale_claim).unwrap();
        recover_stale_claims(&dir);
        assert!(original.exists());
        assert!(!stale_claim.exists());

        // Verify fallback-to-verified migration preserves .bout files even on name collision.
        let verified_user_id = Uuid::new_v4();
        let verified_scope = user_scope_hash(&cfg.app_url, verified_user_id);
        let verified_dir = scope.spool_base_dir().unwrap().join(&verified_scope);
        create_dir_0700(&verified_dir).unwrap();
        fs::write(verified_dir.join(&names[0]), b"{}").unwrap();
        scope.record_verified_user(verified_user_id);

        let migrated: Vec<String> = fs::read_dir(&verified_dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".bout"))
            .collect();
        assert_eq!(migrated.len(), 2);
    }

    #[tokio::test]
    async fn submit_or_spool_shadow_bout_spools_on_404() {
        use crate::arena::shadow::{ShadowBoutMatrixInput, ShadowBoutSideInput};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, axum::Router::new()).await.unwrap();
        });

        let tmp = tempfile::tempdir().unwrap();
        let cfg = ClientConfig {
            app_url: format!("http://{addr}"),
            token: "pat_old_server".to_string(),
        };
        let client = ApiClient::new(&cfg).unwrap();
        let scope = SpoolScope::with_base_dir(&cfg, Some(tmp.path().to_path_buf()));
        let bout = ShadowBoutInput {
            bout_key: "sb_404_1".to_string(),
            track: "coding".to_string(),
            champion_source: "headless".to_string(),
            champion_human_turns: 0,
            repo: "robert-crandall/demo".to_string(),
            base_sha: "abc123".to_string(),
            task_ref: "PR #9".to_string(),
            task_kind: "feature".to_string(),
            prompt_sha256: "hash".to_string(),
            champion: ShadowBoutSideInput {
                model: "claude-opus-5.5".to_string(),
                status: "ok".to_string(),
                metrics: serde_json::json!({}),
                ..Default::default()
            },
            challenger: ShadowBoutSideInput {
                model: "gpt-6.2-sol".to_string(),
                status: "ok".to_string(),
                metrics: serde_json::json!({}),
                ..Default::default()
            },
            matrix: ShadowBoutMatrixInput::default(),
            winner: String::new(),
            judge_model: String::new(),
            judge_lineage: String::new(),
            judge_rationale: String::new(),
            judged_at: None,
            occurred_at: None,
            client_sent_at: None,
            queue_only: false,
        };

        let out = submit_or_spool_shadow_bout(&client, &scope, bout)
            .await
            .expect("404 from older server should spool instead of failing");
        assert!(out.spooled);
        assert_eq!(out.bout_key, "sb_404_1");

        let dir = scope.active_spool_dir().unwrap();
        let count = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".bout"))
            .count();
        assert_eq!(count, 1);

        server.abort();
    }
}
