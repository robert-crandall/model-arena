use std::collections::HashSet;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use utoipa::{IntoParams, ToSchema};

pub const SKILL_ADVERSARIAL_REVIEW: &str = "adversarial-review";
pub const SKILL_PLAN_TOURNAMENT: &str = "plan-tournament";
pub const VALID_SKILLS: [&str; 2] = [SKILL_ADVERSARIAL_REVIEW, SKILL_PLAN_TOURNAMENT];

pub const TOURNAMENT_LENSES: [&str; 3] = ["minimal_diff", "clean_architecture", "edge_cases"];

pub const VALID_ARTIFACT_KINDS: [&str; 5] = ["pr_diff", "plan", "patch", "branch_diff", "other"];
pub const VALID_PHASES: [&str; 3] = ["independent", "targeted", "follow_up"];
pub const VALID_APPROACH_VERDICTS: [&str; 5] =
    ["", "keep", "adjust", "rethink", "insufficient_evidence"];
pub const VALID_WINNING_ROLES: [&str; 4] = ["", "minimal_diff", "clean_architecture", "edge_cases"];
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
pub const VALID_CANDIDATE_SEVERITIES: [&str; 3] = ["blocking", "important", "minor"];
pub const VALID_CANDIDATE_DISPOSITIONS: [&str; 3] = ["adopted", "set_aside", "open_question"];
pub const VALID_TASK_KINDS: [&str; 5] = ["feature", "bugfix", "refactor", "cleanup", "other"];
pub const VALID_SIZE_BUCKETS: [&str; 3] = ["S", "M", "L"];
pub const VALID_REASONING_EFFORTS: [&str; 7] =
    ["", "minimal", "low", "medium", "high", "xhigh", "max"];
pub const VALID_TRIAL_DECISIONS: [&str; 2] = ["promote", "retire"];
pub const VALID_HUMAN_OVERRIDES: [&str; 3] = ["confirmed", "overturned", "clear"];

/// Attaches a JSON Schema `enum` constraint derived from a single-source `VALID_*` constant.
/// Appends `null` automatically for nullable (`Option<String>`) fields and reconciles `default`
/// so empty-string serde defaults are never advertised outside the enum.
#[derive(Clone, Copy)]
pub struct StringEnum {
    values: &'static [&'static str],
    default: Option<&'static str>,
}

impl StringEnum {
    pub const fn new(values: &'static [&'static str]) -> Self {
        Self {
            values,
            default: None,
        }
    }

    pub const fn with_default(values: &'static [&'static str], default: &'static str) -> Self {
        Self {
            values,
            default: Some(default),
        }
    }
}

impl schemars::transform::Transform for StringEnum {
    fn transform(&mut self, schema: &mut schemars::Schema) {
        let nullable = schema
            .get("type")
            .and_then(|v| v.as_array())
            .is_some_and(|arr| arr.iter().any(|t| t.as_str() == Some("null")));
        let mut allowed: Vec<serde_json::Value> = self
            .values
            .iter()
            .map(|v| serde_json::Value::String((*v).to_string()))
            .collect();
        if nullable {
            allowed.push(serde_json::Value::Null);
        }
        schema.insert("enum".into(), serde_json::Value::Array(allowed));
        if let Some(default) = self.default {
            schema.insert(
                "default".into(),
                serde_json::Value::String(default.to_string()),
            );
        } else if !self.values.contains(&"")
            && schema.get("default").and_then(|v| v.as_str()) == Some("")
        {
            schema.remove("default");
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, sqlx::FromRow, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RosterEntry {
    pub skill: String,
    pub lineage: String,
    pub champion_model: String,
    #[serde(default)]
    pub champion_reasoning_effort: String,
    pub backup_model: String,
    #[serde(default)]
    pub backup_reasoning_effort: String,
    pub sort_order: i32,
    pub enabled: bool,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[schemars(inline)]
pub struct RosterSlotInput {
    /// Model lineage (for example: openai, anthropic, google).
    pub lineage: String,
    pub champion_model: String,
    #[serde(default)]
    #[schemars(transform = StringEnum::new(&VALID_REASONING_EFFORTS))]
    pub champion_reasoning_effort: String,
    #[serde(default)]
    pub backup_model: String,
    #[serde(default)]
    #[schemars(transform = StringEnum::new(&VALID_REASONING_EFFORTS))]
    pub backup_reasoning_effort: String,
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
pub struct RosterListResponse {
    pub roster: Vec<RosterEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReplaceRosterRequest {
    pub slots: Vec<RosterSlotInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TrialsListResponse {
    pub trials: Vec<Trial>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Trial {
    pub id: i64,
    pub skill: String,
    pub lineage: String,
    pub champion_model: String,
    #[serde(default)]
    pub champion_reasoning_effort: String,
    pub challenger_model: String,
    #[serde(default)]
    pub challenger_reasoning_effort: String,
    pub target_runs: i32,
    pub completed_runs: i32,
    pub swap_percent: i32,
    pub allow_pr_diff: bool,
    pub status: String,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StartTrialInput {
    /// Skill for the bout: adversarial-review or plan-tournament.
    #[schemars(transform = StringEnum::new(&VALID_SKILLS))]
    pub skill: String,
    /// Roster lineage (for example: openai, anthropic, google).
    pub lineage: String,
    pub challenger_model: String,
    #[serde(default)]
    #[schemars(transform = StringEnum::new(&VALID_REASONING_EFFORTS))]
    pub challenger_reasoning_effort: String,
    pub target_runs: i32,
    #[serde(default)]
    pub swap_percent: Option<i32>,
    #[serde(default)]
    pub allow_pr_diff: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DecideTrialInput {
    /// Decision: promote or retire.
    #[schemars(transform = StringEnum::new(&VALID_TRIAL_DECISIONS))]
    pub decision: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SelectPanelInput {
    /// Skill to select a panel for: adversarial-review or plan-tournament.
    #[schemars(transform = StringEnum::new(&VALID_SKILLS))]
    pub skill: String,
    /// Artifact kind: pr_diff, plan, patch, branch_diff, or other.
    #[serde(default)]
    #[schemars(transform = StringEnum::new(&VALID_ARTIFACT_KINDS))]
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
    pub reasoning_effort: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub backup_model: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub backup_reasoning_effort: String,
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

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[schemars(inline)]
pub struct RunLegInput {
    /// Zero-based distinct slot index for this leg.
    pub slot_index: i32,
    /// Leg role: reviewer or ponytail for adversarial-review; minimal_diff, clean_architecture, edge_cases, critic, or explore_scout for plan-tournament.
    #[schemars(transform = StringEnum::new(&VALID_LEG_ROLES))]
    pub role: String,
    /// Model lineage (for example: openai, anthropic, google).
    #[serde(default)]
    pub lineage: String,
    /// Executed model ID for this leg.
    pub model: String,
    #[serde(default)]
    #[schemars(transform = StringEnum::new(&VALID_REASONING_EFFORTS))]
    pub reasoning_effort: String,
    #[serde(default)]
    pub is_challenger: bool,
    #[serde(default)]
    pub trial_id: Option<i64>,
    /// Execution status (defaults to ok).
    #[serde(default)]
    #[schemars(transform = StringEnum::with_default(&VALID_LEG_STATUSES, "ok"))]
    pub status: String,
    #[serde(default)]
    pub duration_ms: i32,
    /// True on the winning planner leg in plan-tournament runs.
    #[serde(default)]
    pub won_core_plan: bool,
    /// Count of ideas cherry-picked from this planner leg in plan-tournament runs.
    #[serde(default)]
    pub cherry_picks_count: i32,
    #[serde(default)]
    pub critic_adopted_count: i32,
    #[serde(default)]
    pub critic_bloat_dropped_count: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[schemars(inline)]
pub struct CandidateInput {
    #[serde(default)]
    pub candidate_key: String,
    #[serde(default)]
    pub summary: String,
    /// Finding severity (defaults to important).
    #[serde(default)]
    #[schemars(transform = StringEnum::with_default(&VALID_CANDIDATE_SEVERITIES, "important"))]
    pub severity: String,
    /// Triage disposition: adopted, set_aside, or open_question.
    #[schemars(transform = StringEnum::new(&VALID_CANDIDATE_DISPOSITIONS))]
    pub disposition: String,
    #[serde(default)]
    pub caused_approach_pivot: bool,
    /// Reviewer model IDs (or "ponytail") that proposed this finding.
    pub proposing_models: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecordRunInput {
    pub run_key: String,
    /// Skill being recorded: adversarial-review or plan-tournament.
    #[schemars(transform = StringEnum::new(&VALID_SKILLS))]
    pub skill: String,
    /// Artifact kind: pr_diff, plan, patch (local edits), branch_diff, or other (default).
    #[serde(default)]
    #[schemars(transform = StringEnum::with_default(&VALID_ARTIFACT_KINDS, "other"))]
    pub artifact_kind: String,
    #[serde(default)]
    pub artifact_ref: String,
    #[serde(default)]
    pub repo: String,
    /// Review phase: independent (default), targeted, or follow_up.
    #[serde(default)]
    #[schemars(transform = StringEnum::with_default(&VALID_PHASES, "independent"))]
    pub phase: String,
    #[serde(default)]
    pub host_model: String,
    #[serde(default)]
    #[schemars(transform = StringEnum::new(&VALID_REASONING_EFFORTS))]
    pub host_reasoning_effort: String,
    #[serde(default)]
    pub host_lineage: String,
    #[serde(default)]
    pub harness: String,
    #[serde(default)]
    pub machine: String,
    /// Approach verdict for adversarial-review runs (omit or leave empty for plan-tournament).
    #[serde(default)]
    #[schemars(transform = StringEnum::new(&VALID_APPROACH_VERDICTS))]
    pub approach_verdict: String,
    /// Winning planner role for plan-tournament runs (omit or leave empty for adversarial-review).
    #[serde(default)]
    #[schemars(transform = StringEnum::new(&VALID_WINNING_ROLES))]
    pub winning_role: String,
    #[serde(default)]
    #[schemars(with = "Option<String>")]
    pub occurred_at: Option<DateTime<Utc>>,
    #[serde(default)]
    #[schemars(with = "Option<String>")]
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

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OverrideCandidateInput {
    /// Override value: confirmed, overturned, or clear.
    #[schemars(transform = StringEnum::new(&VALID_HUMAN_OVERRIDES))]
    pub human_override: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OverrideCandidateResponse {
    pub id: i64,
    pub human_override: String,
}

#[derive(
    Debug, Clone, Default, Serialize, Deserialize, ToSchema, IntoParams, JsonSchema, PartialEq, Eq,
)]
#[serde(rename_all = "camelCase")]
pub struct LeaderboardQuery {
    /// Lookback window in days (1..=3650, defaults to 90)
    pub days: Option<i32>,
    /// Optional filter: adversarial-review or plan-tournament
    #[schemars(transform = StringEnum::new(&VALID_SKILLS))]
    pub skill: Option<String>,
    /// Optional owner/repo filter
    pub repo: Option<String>,
    /// Optional RFC3339 lower bound timestamp (overrides days)
    #[schemars(with = "Option<String>")]
    pub from: Option<DateTime<Utc>>,
    /// Optional RFC3339 upper bound timestamp
    #[schemars(with = "Option<String>")]
    pub to: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelScorecard {
    pub model: String,
    #[serde(default)]
    pub reasoning_effort: String,
    pub lineage: String,
    pub is_current_champion: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_challenger_trial_id: Option<i64>,
    pub review_runs: i32,
    pub solo_catches: i32,
    pub shared_catches: i32,
    pub set_aside_count: i32,
    pub open_question_count: i32,
    pub approach_pivots: i32,
    pub precision: f64,
    pub solo_catch_rate: f64,
    pub planner_runs: i32,
    pub core_plan_wins: i32,
    pub cherry_picks: i32,
    pub debiased_tournament_score: f64,
    pub critic_runs: i32,
    pub critic_adopted: i32,
    pub critic_bloat_dropped: i32,
    pub failures_count: i32,
    pub retried_count: i32,
    pub avg_duration_ms: i32,
    pub timed_runs: i32,
    pub p50_duration_ms: i32,
    pub p90_duration_ms: i32,
    pub straggler_count: i32,
    pub straggler_rate: f64,
    pub catches_per_minute: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LensCell {
    pub model: String,
    #[serde(default)]
    pub reasoning_effort: String,
    pub lens: String,
    pub runs: i32,
    pub wins: i32,
    pub win_rate: f64,
    pub cherry_picks: i32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HostBiasCell {
    pub host_model: String,
    pub host_lineage: String,
    pub reviewer_model: String,
    pub reviewer_lineage: String,
    pub candidates_evaluated: i32,
    pub adopted_count: i32,
    pub adoption_rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TrialComparison {
    pub trial: Trial,
    pub champion: ModelScorecard,
    pub challenger: ModelScorecard,
    pub verdict_hint: String,
    pub verdict_notes: String,
    pub p50_delta_ms: i32,
    pub speedup_ratio: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RunCandidateView {
    pub id: i64,
    pub candidate_index: i32,
    pub summary: String,
    pub severity: String,
    pub disposition: String,
    pub effective_disposition: String,
    pub caused_approach_pivot: bool,
    pub proposing_models: Vec<String>,
    pub human_override: String,
    pub override_note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RunLegView {
    pub slot_index: i32,
    pub role: String,
    pub lineage: String,
    pub model: String,
    #[serde(default)]
    pub reasoning_effort: String,
    pub is_challenger: bool,
    pub status: String,
    pub duration_ms: i32,
    pub won_core_plan: bool,
    pub cherry_picks_count: i32,
    pub critic_adopted_count: i32,
    pub critic_bloat_dropped_count: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecentRunView {
    pub id: i64,
    pub run_key: String,
    pub skill: String,
    pub artifact_kind: String,
    pub artifact_ref: String,
    pub repo: String,
    pub host_model: String,
    #[serde(default)]
    pub host_reasoning_effort: String,
    pub harness: String,
    pub machine: String,
    pub approach_verdict: String,
    pub winning_role: String,
    pub occurred_at: DateTime<Utc>,
    pub legs: Vec<RunLegView>,
    pub candidates: Vec<RunCandidateView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Leaderboard {
    pub window_days: i32,
    pub total_runs: i32,
    pub roster: Vec<RosterEntry>,
    pub models: Vec<ModelScorecard>,
    pub lens_matrix: Vec<LensCell>,
    pub host_bias_matrix: Vec<HostBiasCell>,
    pub trials: Vec<TrialComparison>,
    pub recent_runs: Vec<RecentRunView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodingRunInput {
    pub run_key: String,
    pub model: String,
    #[serde(default)]
    #[schemars(transform = StringEnum::new(&VALID_REASONING_EFFORTS))]
    pub reasoning_effort: String,
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
    /// Task kind: feature (default), bugfix, refactor, cleanup, or other.
    #[serde(default)]
    #[schemars(transform = StringEnum::with_default(&VALID_TASK_KINDS, "feature"))]
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
    #[schemars(with = "Option<String>")]
    pub occurred_at: Option<DateTime<Utc>>,
    #[serde(default)]
    #[schemars(with = "Option<String>")]
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
    #[serde(default)]
    pub reasoning_effort: String,
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
pub struct RecordedCodingRunOutput {
    pub run_id: i64,
    pub run_key: String,
    pub size_bucket: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub spooled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OverrideCodingRunInput {
    /// Override value: `confirmed` (marks the PR outcome clean and clears `escapedRegression`),
    /// `overturned` (marks the PR outcome as having an escaped regression), or `clear` (resets to recorded metrics).
    #[schemars(transform = StringEnum::new(&VALID_HUMAN_OVERRIDES))]
    pub human_override: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OverrideCodingRunResponse {
    pub id: i64,
    pub human_override: String,
}

#[derive(
    Debug, Clone, Default, Serialize, Deserialize, ToSchema, IntoParams, JsonSchema, PartialEq, Eq,
)]
#[serde(rename_all = "camelCase")]
pub struct CodingLeaderboardQuery {
    /// Lookback window in days (1..=3650, defaults to 90)
    pub days: Option<i32>,
    /// Optional owner/repo filter
    pub repo: Option<String>,
    /// Optional task kind filter: feature, bugfix, refactor, cleanup, other
    #[schemars(transform = StringEnum::new(&VALID_TASK_KINDS))]
    pub task_kind: Option<String>,
    /// Optional diff size bucket filter: S (<50 LOC), M (50..=250 LOC), L (>250 LOC)
    #[schemars(transform = StringEnum::new(&VALID_SIZE_BUCKETS))]
    pub size_bucket: Option<String>,
    /// Optional RFC3339 lower bound timestamp (overrides days)
    #[schemars(with = "Option<String>")]
    pub from: Option<DateTime<Utc>>,
    /// Optional RFC3339 upper bound timestamp
    #[schemars(with = "Option<String>")]
    pub to: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CodingModelProfile {
    pub model: String,
    #[serde(default)]
    pub reasoning_effort: String,
    pub lineage: String,
    pub total_runs: i32,
    pub merged_runs: i32,
    pub abandoned_runs: i32,
    pub zero_touch_runs: i32,
    pub zero_touch_rate: f64,
    pub first_pass_clean_runs: i32,
    pub first_pass_clean_rate: f64,
    pub merged_clean_24h_runs: i32,
    pub merged_clean_24h_rate: f64,
    pub escaped_regressions: i32,
    pub escaped_regression_rate: f64,
    pub avg_prompt_turns: f64,
    pub avg_red_ci_pushes: f64,
    pub avg_ccr_rounds: f64,
    pub total_ccr_actionable: i32,
    pub total_ccr_noise: i32,
    pub ccr_noise_ratio: f64,
    pub total_human_threads: i32,
    pub avg_human_threads: f64,
    pub avg_loc_churn: i32,
    pub avg_tests_delta: f64,
    pub p50_time_to_green_sec: i32,
    pub p90_time_to_green_sec: i32,
    pub p50_time_to_merge_sec: i32,
    pub p90_time_to_merge_sec: i32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CodingSliceCell {
    pub model: String,
    #[serde(default)]
    pub reasoning_effort: String,
    pub size_bucket: String,
    pub task_kind: String,
    pub runs: i32,
    pub zero_touch_rate: f64,
    pub first_pass_clean_rate: f64,
    pub escaped_regression_rate: f64,
    pub avg_ccr_rounds: f64,
    pub p50_time_to_merge_sec: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CodingLeaderboard {
    pub window_days: i32,
    pub total_runs: i32,
    pub models: Vec<CodingModelProfile>,
    pub slices: Vec<CodingSliceCell>,
    pub recent_runs: Vec<CodingRunRecord>,
}

pub fn is_valid_skill(skill: &str) -> bool {
    matches!(skill, SKILL_ADVERSARIAL_REVIEW | SKILL_PLAN_TOURNAMENT)
}

pub fn normalize_reasoning_effort(raw: &str) -> Result<String, String> {
    let norm = raw.trim().to_ascii_lowercase();
    if VALID_REASONING_EFFORTS.contains(&norm.as_str()) {
        Ok(norm)
    } else {
        Err(format!(
            "invalid reasoningEffort {raw:?}: must be one of {:?}",
            VALID_REASONING_EFFORTS
        ))
    }
}

pub fn model_effort_key(model: &str, reasoning_effort: &str) -> (String, String) {
    (
        model.trim().to_ascii_lowercase(),
        reasoning_effort.trim().to_ascii_lowercase(),
    )
}

pub fn is_valid_artifact_kind(kind: &str) -> bool {
    VALID_ARTIFACT_KINDS.contains(&kind)
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
            champion_reasoning_effort: String::new(),
            backup_model: "custom-haiku".to_string(),
            backup_reasoning_effort: String::new(),
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
