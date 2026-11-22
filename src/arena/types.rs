use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;

pub const SKILL_ADVERSARIAL_REVIEW: &str = "adversarial-review";
pub const SKILL_PLAN_TOURNAMENT: &str = "plan-tournament";

pub const TOURNAMENT_LENSES: [&str; 3] = ["minimal_diff", "clean_architecture", "edge_cases"];

pub const VALID_ARTIFACT_KINDS: [&str; 5] = ["pr_diff", "plan", "patch", "branch_diff", "other"];
pub const VALID_PHASES: [&str; 3] = ["independent", "targeted", "follow_up"];
pub const VALID_APPROACH_VERDICTS: [&str; 5] =
    ["", "keep", "adjust", "rethink", "insufficient_evidence"];
pub const VALID_LEG_ROLES: [&str; 7] = [
    "reviewer",
    "ponytail",
    "explore_scout",
    "minimal_diff",
    "clean_architecture",
    "edge_cases",
    "critic",
];
pub const VALID_LEG_STATUSES: [&str; 5] = ["ok", "retried", "fallback", "timeout", "skipped"];
pub const VALID_CANDIDATE_DISPOSITIONS: [&str; 3] = ["adopted", "set_aside", "open_question"];
pub const VALID_TASK_KINDS: [&str; 5] = ["feature", "bugfix", "refactor", "cleanup", "other"];
pub const VALID_SIZE_BUCKETS: [&str; 3] = ["S", "M", "L"];

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, sqlx::FromRow, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RosterEntry {
    pub skill: String,
    pub lineage: String,
    pub champion_model: String,
    pub backup_model: String,
    pub sort_order: i32,
    pub enabled: bool,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RosterSlotInput {
    pub lineage: String,
    pub champion_model: String,
    #[serde(default)]
    pub backup_model: String,
    #[serde(default)]
    pub sort_order: i32,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Trial {
    pub id: i64,
    pub skill: String,
    pub lineage: String,
    pub champion_model: String,
    pub challenger_model: String,
    pub target_runs: i32,
    pub completed_runs: i32,
    pub swap_percent: i32,
    pub allow_pr_diff: bool,
    pub status: String,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StartTrialInput {
    pub skill: String,
    pub lineage: String,
    pub challenger_model: String,
    pub target_runs: i32,
    #[serde(default)]
    pub swap_percent: Option<i32>,
    #[serde(default)]
    pub allow_pr_diff: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DecideTrialInput {
    pub decision: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SelectPanelInput {
    pub skill: String,
    #[serde(default)]
    pub artifact_kind: Option<String>,
    #[serde(default)]
    pub host_model: Option<String>,
    #[serde(default)]
    pub host_lineage: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PanelLeg {
    pub slot_index: i32,
    pub role: String,
    pub lineage: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub backup_model: String,
    pub is_challenger: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trial_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PanelSelection {
    pub configured: bool,
    pub run_key: String,
    pub skill: String,
    pub artifact_kind: String,
    pub host_lineage: String,
    pub rotation_turn: i64,
    pub legs: Vec<PanelLeg>,
    pub critic_order: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RunLegInput {
    pub slot_index: i32,
    pub role: String,
    #[serde(default)]
    pub lineage: String,
    pub model: String,
    #[serde(default)]
    pub is_challenger: bool,
    #[serde(default)]
    pub trial_id: Option<i64>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub duration_ms: i32,
    #[serde(default)]
    pub won_core_plan: bool,
    #[serde(default)]
    pub cherry_picks_count: i32,
    #[serde(default)]
    pub critic_adopted_count: i32,
    #[serde(default)]
    pub critic_bloat_dropped_count: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CandidateInput {
    #[serde(default)]
    pub candidate_key: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub severity: String,
    pub disposition: String,
    #[serde(default)]
    pub caused_approach_pivot: bool,
    pub proposing_models: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecordRunInput {
    pub run_key: String,
    pub skill: String,
    #[serde(default)]
    pub artifact_kind: String,
    #[serde(default)]
    pub artifact_ref: String,
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub phase: String,
    #[serde(default)]
    pub host_model: String,
    #[serde(default)]
    pub host_lineage: String,
    #[serde(default)]
    pub harness: String,
    #[serde(default)]
    pub machine: String,
    #[serde(default)]
    pub approach_verdict: String,
    #[serde(default)]
    pub winning_role: String,
    #[serde(default)]
    pub occurred_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub client_sent_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub queue_only: bool,
    pub legs: Vec<RunLegInput>,
    #[serde(default)]
    pub candidates: Vec<CandidateInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecordedRunOutput {
    pub run_id: i64,
    pub run_key: String,
    pub completed_trials: Vec<Trial>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub spooled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OverrideCandidateInput {
    pub human_override: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodingRunInput {
    pub run_key: String,
    pub model: String,
    #[serde(default)]
    pub lineage: String,
    #[serde(default)]
    pub harness: String,
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub pr_number: i32,
    #[serde(default)]
    pub issue_ref: String,
    #[serde(default)]
    pub task_kind: String,
    #[serde(default)]
    pub loc_added: i32,
    #[serde(default)]
    pub loc_deleted: i32,
    #[serde(default)]
    pub files_changed: i32,
    #[serde(default)]
    pub tests_delta: i32,
    #[serde(default)]
    pub prompt_turn_count: i32,
    #[serde(default)]
    pub red_ci_pushes: i32,
    #[serde(default)]
    pub ccr_rounds: i32,
    #[serde(default)]
    pub ccr_actionable_count: i32,
    #[serde(default)]
    pub ccr_noise_count: i32,
    #[serde(default)]
    pub human_threads_count: i32,
    #[serde(default)]
    pub escaped_regression: bool,
    #[serde(default)]
    pub merged_without_followup_24h: bool,
    #[serde(default)]
    pub first_pass_clean: bool,
    #[serde(default)]
    pub abandoned: bool,
    #[serde(default)]
    pub time_to_green_sec: i32,
    #[serde(default)]
    pub time_to_merge_sec: i32,
    #[serde(default)]
    pub digest_summary: String,
    #[serde(default)]
    pub occurred_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub client_sent_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub queue_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, sqlx::FromRow, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodingRunRecord {
    pub id: i64,
    pub run_key: String,
    pub model: String,
    pub lineage: String,
    pub harness: String,
    pub repo: String,
    pub pr_number: i32,
    pub issue_ref: String,
    pub task_kind: String,
    pub loc_added: i32,
    pub loc_deleted: i32,
    pub size_bucket: String,
    pub files_changed: i32,
    pub tests_delta: i32,
    pub prompt_turn_count: i32,
    pub red_ci_pushes: i32,
    pub ccr_rounds: i32,
    pub ccr_actionable_count: i32,
    pub ccr_noise_count: i32,
    pub human_threads_count: i32,
    pub escaped_regression: bool,
    pub merged_without_followup_24h: bool,
    pub first_pass_clean: bool,
    pub abandoned: bool,
    pub time_to_green_sec: i32,
    pub time_to_merge_sec: i32,
    pub digest_summary: String,
    pub human_override: String,
    pub override_note: String,
    pub occurred_at: DateTime<Utc>,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OverrideCodingRunInput {
    pub human_override: String,
    #[serde(default)]
    pub note: String,
}

pub fn is_valid_skill(skill: &str) -> bool {
    matches!(skill, SKILL_ADVERSARIAL_REVIEW | SKILL_PLAN_TOURNAMENT)
}

pub fn is_trial_competing_role(skill: &str, role: &str) -> bool {
    match skill {
        SKILL_ADVERSARIAL_REVIEW => role == "reviewer",
        SKILL_PLAN_TOURNAMENT => {
            matches!(role, "minimal_diff" | "clean_architecture" | "edge_cases")
        }
        _ => false,
    }
}

pub fn infer_lineage(model: &str, roster: &[RosterEntry]) -> String {
    let m = model.trim().to_ascii_lowercase();
    if m.is_empty() {
        return String::new();
    }
    for r in roster {
        if r.champion_model.eq_ignore_ascii_case(&m) || r.backup_model.eq_ignore_ascii_case(&m) {
            return r.lineage.to_ascii_lowercase();
        }
    }
    if m.starts_with("gpt-") || m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") {
        "openai".to_string()
    } else if m.starts_with("claude-") {
        "anthropic".to_string()
    } else if m.starts_with("gemini-") {
        "google".to_string()
    } else if m.starts_with("grok-") {
        "xai".to_string()
    } else if m.starts_with("mai-") {
        "microsoft".to_string()
    } else if let Some((prefix, _)) = m.split_once('-')
        && !prefix.is_empty()
    {
        prefix.to_string()
    } else {
        m
    }
}

pub fn classify_size_bucket(loc_added: i32, loc_deleted: i32) -> &'static str {
    let total = loc_added.saturating_add(loc_deleted);
    if total < 50 {
        "S"
    } else if total <= 250 {
        "M"
    } else {
        "L"
    }
}

/// Derives deterministic candidate keys (`ck_<sha256[:24]>`) when `candidate_key` is omitted,
/// disambiguating collisions with `#2`, `#3`, etc., and rejecting duplicate explicit keys.
pub fn derive_candidate_keys(candidates: &[CandidateInput]) -> Result<Vec<String>, String> {
    let mut explicit_seen = HashSet::with_capacity(candidates.len());
    for c in candidates {
        let key = c.candidate_key.trim();
        if !key.is_empty() && !explicit_seen.insert(key.to_string()) {
            return Err(format!("duplicate candidateKey {key:?}"));
        }
    }

    let mut used = explicit_seen;
    let mut out = Vec::with_capacity(candidates.len());

    for c in candidates {
        let explicit = c.candidate_key.trim();
        if !explicit.is_empty() {
            out.push(explicit.to_string());
            continue;
        }

        let norm_summary = c
            .summary
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        let mut models: Vec<String> = c
            .proposing_models
            .iter()
            .map(|m| m.trim().to_lowercase())
            .filter(|m| !m.is_empty())
            .collect();
        models.sort();
        models.dedup();

        let mut hasher = Sha256::new();
        hasher.update(norm_summary.as_bytes());
        hasher.update(b"\0");
        hasher.update(models.join(",").as_bytes());
        let hex_digest = hex::encode(hasher.finalize());
        let base_key = format!("ck_{}", &hex_digest[..24]);

        let mut key = base_key.clone();
        let mut suffix = 2usize;
        while used.contains(&key) {
            key = format!("{base_key}#{suffix}");
            suffix += 1;
        }
        used.insert(key.clone());
        out.push(key);
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infer_lineage_maps_well_known_prefixes_and_roster() {
        let roster = vec![RosterEntry {
            skill: SKILL_ADVERSARIAL_REVIEW.to_string(),
            lineage: "anthropic".to_string(),
            champion_model: "custom-sonnet".to_string(),
            backup_model: "custom-haiku".to_string(),
            sort_order: 0,
            enabled: true,
            updated_at: Utc::now(),
        }];

        assert_eq!(infer_lineage("gpt-6.1-sol", &[]), "openai");
        assert_eq!(infer_lineage("o3-mini", &[]), "openai");
        assert_eq!(infer_lineage("claude-opus-5.5", &[]), "anthropic");
        assert_eq!(infer_lineage("gemini-4-argon", &[]), "google");
        assert_eq!(infer_lineage("grok-4.6", &[]), "xai");
        assert_eq!(infer_lineage("mai-code-2", &[]), "microsoft");
        assert_eq!(infer_lineage("Custom-Haiku", &roster), "anthropic");
        assert_eq!(infer_lineage("  ", &roster), "");
    }

    #[test]
    fn classify_size_bucket_uses_total_loc_thresholds() {
        assert_eq!(classify_size_bucket(0, 0), "S");
        assert_eq!(classify_size_bucket(30, 19), "S");
        assert_eq!(classify_size_bucket(30, 20), "M");
        assert_eq!(classify_size_bucket(200, 50), "M");
        assert_eq!(classify_size_bucket(200, 51), "L");
    }

    #[test]
    fn derive_candidate_keys_is_deterministic_and_disambiguates_collisions() {
        let c1 = CandidateInput {
            candidate_key: String::new(),
            summary: "  Race   condition in   pool ".to_string(),
            severity: "blocking".to_string(),
            disposition: "adopted".to_string(),
            caused_approach_pivot: false,
            proposing_models: vec!["claude-opus-5.5".to_string(), "gpt-6.1-sol".to_string()],
        };
        let c2 = CandidateInput {
            candidate_key: String::new(),
            summary: "race condition in pool".to_string(),
            severity: "minor".to_string(),
            disposition: "set_aside".to_string(),
            caused_approach_pivot: false,
            proposing_models: vec!["GPT-6.1-SOL".to_string(), "Claude-Opus-5.5".to_string()],
        };

        let keys = derive_candidate_keys(&[c1, c2]).expect("keys derived");
        assert_eq!(keys.len(), 2);
        assert!(keys[0].starts_with("ck_"));
        assert_eq!(keys[1], format!("{}#2", keys[0]));

        let dup = vec![
            CandidateInput {
                candidate_key: "explicit-1".to_string(),
                summary: "a".to_string(),
                severity: String::new(),
                disposition: "adopted".to_string(),
                caused_approach_pivot: false,
                proposing_models: vec!["gpt-6.1-sol".to_string()],
            },
            CandidateInput {
                candidate_key: " explicit-1 ".to_string(),
                summary: "b".to_string(),
                severity: String::new(),
                disposition: "adopted".to_string(),
                caused_approach_pivot: false,
                proposing_models: vec!["gpt-6.1-sol".to_string()],
            },
        ];
        assert!(derive_candidate_keys(&dup).is_err());
    }
}
