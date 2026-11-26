use std::collections::HashMap;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use sqlx::types::Json as SqlxJson;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::arena::service::lock_user_row;
use crate::arena::types::{
    ReasoningEffortSchema, StringEnum, VALID_REASONING_EFFORTS, VALID_TASK_KINDS, infer_lineage,
    model_effort_key, normalize_reasoning_effort,
};
use crate::routes::ApiError;

pub const SHADOW_TRACK_CODING: &str = "coding";
pub const SHADOW_TRACK_ORCHESTRATION: &str = "orchestration";
pub const VALID_SHADOW_TRACKS: [&str; 2] = [SHADOW_TRACK_CODING, SHADOW_TRACK_ORCHESTRATION];

pub const CHAMPION_SOURCE_HEADLESS: &str = "headless";
pub const CHAMPION_SOURCE_EXISTING_REF: &str = "existing_ref";
pub const VALID_CHAMPION_SOURCES: [&str; 2] =
    [CHAMPION_SOURCE_HEADLESS, CHAMPION_SOURCE_EXISTING_REF];

pub const VALID_SHADOW_TEST_CELLS: [&str; 6] = [
    "pass",
    "fail",
    "compile_error",
    "no_tests",
    "timeout",
    "skipped",
];

pub const VALID_SHADOW_WINNERS: [&str; 5] = ["", "champion", "challenger", "tie", "both_failed"];

pub const VALID_SHADOW_LEG_STATUSES: [&str; 5] =
    ["ok", "timeout", "credit_cap", "failed", "imported"];

fn default_headless() -> String {
    CHAMPION_SOURCE_HEADLESS.to_string()
}

fn default_task_kind() -> String {
    "other".to_string()
}

fn default_skipped() -> String {
    "skipped".to_string()
}

fn default_leg_status() -> String {
    "ok".to_string()
}

fn default_empty_object() -> serde_json::Value {
    serde_json::json!({})
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
#[schemars(inline)]
pub struct ShadowBoutSideInput {
    /// Executed model ID for this side.
    pub model: String,
    #[serde(default)]
    #[schema(inline, value_type = ReasoningEffortSchema)]
    #[schemars(transform = StringEnum::new(&VALID_REASONING_EFFORTS))]
    pub reasoning_effort: String,
    /// Optional lineage override (inferred from model prefix when omitted).
    #[serde(default)]
    pub lineage: String,
    /// Side execution status: ok, timeout, credit_cap, failed, or imported.
    #[serde(default = "default_leg_status")]
    #[schemars(transform = StringEnum::with_default(&VALID_SHADOW_LEG_STATUSES, "ok"))]
    pub status: String,
    #[serde(default)]
    pub duration_ms: i64,
    #[serde(default)]
    pub loc_added: i32,
    #[serde(default)]
    pub loc_deleted: i32,
    #[serde(default)]
    pub files_changed: i32,
    #[serde(default)]
    pub tests_delta: i32,
    #[serde(default)]
    pub usage_available: bool,
    /// Total AI credits consumed in nano-AIU (1 AI credit = 1_000_000_000).
    #[serde(default)]
    pub nano_aiu: i64,
    #[serde(default)]
    pub premium_requests: f64,
    #[serde(default)]
    pub input_tokens: i64,
    #[serde(default)]
    pub output_tokens: i64,
    #[serde(default)]
    pub turn_count: i32,
    #[serde(default)]
    pub subagent_spawns: i32,
    #[serde(default)]
    pub tool_errors: i32,
    /// Extensible trace and usage metadata object.
    #[serde(default = "default_empty_object")]
    pub metrics: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[schemars(inline)]
pub struct ShadowBoutMatrixInput {
    #[serde(default = "default_skipped")]
    #[schemars(transform = StringEnum::with_default(&VALID_SHADOW_TEST_CELLS, "skipped"))]
    pub champion_impl_champion_test: String,
    #[serde(default = "default_skipped")]
    #[schemars(transform = StringEnum::with_default(&VALID_SHADOW_TEST_CELLS, "skipped"))]
    pub challenger_impl_challenger_test: String,
    #[serde(default = "default_skipped")]
    #[schemars(transform = StringEnum::with_default(&VALID_SHADOW_TEST_CELLS, "skipped"))]
    pub champion_impl_challenger_test: String,
    #[serde(default = "default_skipped")]
    #[schemars(transform = StringEnum::with_default(&VALID_SHADOW_TEST_CELLS, "skipped"))]
    pub challenger_impl_champion_test: String,
}

impl Default for ShadowBoutMatrixInput {
    fn default() -> Self {
        Self {
            champion_impl_champion_test: default_skipped(),
            challenger_impl_challenger_test: default_skipped(),
            champion_impl_challenger_test: default_skipped(),
            challenger_impl_champion_test: default_skipped(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ShadowBoutInput {
    /// Stable idempotency key for this paired shadow bout.
    pub bout_key: String,
    /// Evaluation track: coding or orchestration.
    #[schemars(transform = StringEnum::new(&VALID_SHADOW_TRACKS))]
    pub track: String,
    /// Whether Champion ran headlessly from baseSha or was imported from an existing ref/PR.
    #[serde(default = "default_headless")]
    #[schemars(transform = StringEnum::with_default(&VALID_CHAMPION_SOURCES, "headless"))]
    pub champion_source: String,
    /// Number of human steering turns on the Champion side (0 for headless).
    #[serde(default)]
    pub champion_human_turns: i32,
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub base_sha: String,
    #[serde(default)]
    pub task_ref: String,
    #[serde(default = "default_task_kind")]
    #[schemars(transform = StringEnum::with_default(&VALID_TASK_KINDS, "other"))]
    pub task_kind: String,
    #[serde(default)]
    pub prompt_sha256: String,
    pub champion: ShadowBoutSideInput,
    pub challenger: ShadowBoutSideInput,
    #[serde(default)]
    pub matrix: ShadowBoutMatrixInput,
    /// Blinded judgment winner: empty (unjudged), champion, challenger, tie, or both_failed.
    #[serde(default)]
    #[schemars(transform = StringEnum::new(&VALID_SHADOW_WINNERS))]
    pub winner: String,
    #[serde(default)]
    pub judge_model: String,
    #[serde(default)]
    pub judge_lineage: String,
    #[serde(default)]
    pub judge_rationale: String,
    #[serde(default)]
    #[schemars(with = "Option<String>")]
    pub judged_at: Option<DateTime<Utc>>,
    #[serde(default)]
    #[schemars(with = "Option<String>")]
    pub occurred_at: Option<DateTime<Utc>>,
    #[serde(default)]
    #[schemars(with = "Option<String>")]
    pub client_sent_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub queue_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecordedShadowBoutOutput {
    pub bout_id: i64,
    pub bout_key: String,
    pub winner: String,
    pub spooled: bool,
}

#[derive(
    Debug, Clone, Default, Serialize, Deserialize, ToSchema, IntoParams, JsonSchema, PartialEq, Eq,
)]
#[serde(rename_all = "camelCase")]
pub struct ShadowLeaderboardQuery {
    /// Lookback window in days (1..=3650, defaults to 90).
    pub days: Option<i32>,
    /// Optional track filter: coding or orchestration.
    #[schemars(transform = StringEnum::new(&VALID_SHADOW_TRACKS))]
    pub track: Option<String>,
    /// Optional champion source filter: headless or existing_ref.
    #[schemars(transform = StringEnum::new(&VALID_CHAMPION_SOURCES))]
    pub champion_source: Option<String>,
    /// Optional owner/repo filter.
    pub repo: Option<String>,
    /// Optional RFC3339 lower bound timestamp (overrides days).
    #[schemars(with = "Option<String>")]
    pub from: Option<DateTime<Utc>>,
    /// Optional RFC3339 upper bound timestamp.
    #[schemars(with = "Option<String>")]
    pub to: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ShadowModelScorecard {
    pub track: String,
    pub model: String,
    #[serde(default)]
    pub reasoning_effort: String,
    pub lineage: String,
    pub bouts: i32,
    pub judged_bouts: i32,
    pub wins: i32,
    pub losses: i32,
    pub ties: i32,
    pub both_failed: i32,
    pub win_rate: f64,
    pub own_test_pass_rate: f64,
    pub median_credits: f64,
    pub p50_duration_ms: i64,
    pub avg_loc_churn: i32,
    pub avg_turns: f64,
    pub avg_subagent_spawns: f64,
    pub avg_tool_errors: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ShadowPairwiseRecord {
    pub track: String,
    pub model_a: String,
    #[serde(default)]
    pub effort_a: String,
    pub model_b: String,
    #[serde(default)]
    pub effort_b: String,
    pub bouts: i32,
    pub a_wins: i32,
    pub b_wins: i32,
    pub ties: i32,
    pub both_failed: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ShadowBoutSideView {
    pub side: String,
    pub model: String,
    #[serde(default)]
    pub reasoning_effort: String,
    pub lineage: String,
    pub status: String,
    pub duration_ms: i64,
    pub loc_added: i32,
    pub loc_deleted: i32,
    pub files_changed: i32,
    pub tests_delta: i32,
    pub usage_available: bool,
    pub nano_aiu: i64,
    pub credits: f64,
    pub premium_requests: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub turn_count: i32,
    pub subagent_spawns: i32,
    pub tool_errors: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ShadowBoutView {
    pub id: i64,
    pub bout_key: String,
    pub track: String,
    pub champion_source: String,
    pub champion_human_turns: i32,
    pub repo: String,
    pub base_sha: String,
    pub task_ref: String,
    pub task_kind: String,
    pub matrix: ShadowBoutMatrixInput,
    pub winner: String,
    pub judge_model: String,
    pub judge_lineage: String,
    pub judge_rationale: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub judged_at: Option<DateTime<Utc>>,
    pub occurred_at: DateTime<Utc>,
    pub champion: ShadowBoutSideView,
    pub challenger: ShadowBoutSideView,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ShadowLeaderboard {
    pub window_days: i32,
    pub total_bouts: i32,
    pub judged_bouts: i32,
    pub models: Vec<ShadowModelScorecard>,
    pub pairs: Vec<ShadowPairwiseRecord>,
    pub recent_bouts: Vec<ShadowBoutView>,
}

fn validate_side(side_name: &str, side: &mut ShadowBoutSideInput) -> Result<(), ApiError> {
    side.model = side.model.trim().to_string();
    if side.model.is_empty() {
        return Err(ApiError::bad_request(format!(
            "{side_name}.model is required"
        )));
    }
    side.reasoning_effort =
        normalize_reasoning_effort(&side.reasoning_effort).map_err(ApiError::bad_request)?;
    side.lineage = side.lineage.trim().to_ascii_lowercase();
    if side.lineage.is_empty() {
        side.lineage = infer_lineage(&side.model, &[]);
    }
    side.status = side.status.trim().to_ascii_lowercase();
    if side.status.is_empty() {
        side.status = default_leg_status();
    }
    if !VALID_SHADOW_LEG_STATUSES.contains(&side.status.as_str()) {
        return Err(ApiError::bad_request(format!(
            "invalid {side_name}.status {:?}: must be one of {:?}",
            side.status, VALID_SHADOW_LEG_STATUSES
        )));
    }
    if side.duration_ms < 0
        || side.loc_added < 0
        || side.loc_deleted < 0
        || side.files_changed < 0
        || side.nano_aiu < 0
        || side.input_tokens < 0
        || side.output_tokens < 0
        || side.turn_count < 0
        || side.subagent_spawns < 0
        || side.tool_errors < 0
        || !side.premium_requests.is_finite()
        || side.premium_requests < 0.0
    {
        return Err(ApiError::bad_request(format!(
            "{side_name} metrics must be finite and non-negative"
        )));
    }
    if !side.metrics.is_object() {
        return Err(ApiError::bad_request(format!(
            "{side_name}.metrics must be a JSON object"
        )));
    }
    Ok(())
}

fn validate_cell(name: &str, value: &mut String) -> Result<(), ApiError> {
    *value = value.trim().to_ascii_lowercase();
    if value.is_empty() {
        *value = default_skipped();
    }
    if !VALID_SHADOW_TEST_CELLS.contains(&value.as_str()) {
        return Err(ApiError::bad_request(format!(
            "invalid matrix.{name} {:?}: must be one of {:?}",
            value, VALID_SHADOW_TEST_CELLS
        )));
    }
    Ok(())
}

pub fn validate_and_normalize_shadow_bout_input(
    input: &mut ShadowBoutInput,
) -> Result<(), ApiError> {
    input.bout_key = input.bout_key.trim().to_string();
    if input.bout_key.is_empty() || input.bout_key.len() > 200 {
        return Err(ApiError::bad_request(
            "boutKey is required (1..=200 characters)",
        ));
    }
    input.track = input.track.trim().to_ascii_lowercase();
    if !VALID_SHADOW_TRACKS.contains(&input.track.as_str()) {
        return Err(ApiError::bad_request(format!(
            "track must be one of {:?}",
            VALID_SHADOW_TRACKS
        )));
    }
    input.champion_source = input.champion_source.trim().to_ascii_lowercase();
    if input.champion_source.is_empty() {
        input.champion_source = default_headless();
    }
    if !VALID_CHAMPION_SOURCES.contains(&input.champion_source.as_str()) {
        return Err(ApiError::bad_request(format!(
            "championSource must be one of {:?}",
            VALID_CHAMPION_SOURCES
        )));
    }
    if input.champion_human_turns < 0 {
        return Err(ApiError::bad_request("championHumanTurns must be >= 0"));
    }
    input.repo = input.repo.trim().to_string();
    input.base_sha = input.base_sha.trim().to_string();
    input.task_ref = input.task_ref.trim().to_string();
    input.prompt_sha256 = input.prompt_sha256.trim().to_string();
    input.task_kind = input.task_kind.trim().to_ascii_lowercase();
    if input.task_kind.is_empty() {
        input.task_kind = default_task_kind();
    }
    if !VALID_TASK_KINDS.contains(&input.task_kind.as_str()) {
        return Err(ApiError::bad_request(format!(
            "taskKind must be one of {:?}",
            VALID_TASK_KINDS
        )));
    }

    validate_side("champion", &mut input.champion)?;
    validate_side("challenger", &mut input.challenger)?;

    validate_cell(
        "championImplChampionTest",
        &mut input.matrix.champion_impl_champion_test,
    )?;
    validate_cell(
        "challengerImplChallengerTest",
        &mut input.matrix.challenger_impl_challenger_test,
    )?;
    validate_cell(
        "championImplChallengerTest",
        &mut input.matrix.champion_impl_challenger_test,
    )?;
    validate_cell(
        "challengerImplChampionTest",
        &mut input.matrix.challenger_impl_champion_test,
    )?;

    input.winner = input.winner.trim().to_ascii_lowercase();
    if !VALID_SHADOW_WINNERS.contains(&input.winner.as_str()) {
        return Err(ApiError::bad_request(format!(
            "winner must be one of {:?}",
            VALID_SHADOW_WINNERS
        )));
    }
    input.judge_model = input.judge_model.trim().to_string();
    input.judge_lineage = input.judge_lineage.trim().to_ascii_lowercase();
    if input.judge_lineage.is_empty() && !input.judge_model.is_empty() {
        input.judge_lineage = if input.judge_model.eq_ignore_ascii_case("human") {
            "human".to_string()
        } else {
            infer_lineage(&input.judge_model, &[])
        };
    }
    input.judge_rationale = input.judge_rationale.trim().to_string();
    if !input.winner.is_empty() && input.judged_at.is_none() {
        input.judged_at = Some(Utc::now());
    }
    Ok(())
}

#[derive(sqlx::FromRow)]
struct ExistingBoutRow {
    id: i64,
    winner: String,
    judge_model: String,
    judge_lineage: String,
    judge_rationale: String,
    judged_at: Option<DateTime<Utc>>,
    client_sent_at: Option<DateTime<Utc>>,
}

async fn insert_leg(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    bout_id: i64,
    side: &str,
    leg: &ShadowBoutSideInput,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO arena_shadow_bout_legs (
            bout_id, side, model, reasoning_effort, lineage, status,
            duration_ms, loc_added, loc_deleted, files_changed, tests_delta,
            usage_available, nano_aiu, premium_requests, input_tokens, output_tokens,
            turn_count, subagent_spawns, tool_errors, metrics_json
         )
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20)",
    )
    .bind(bout_id)
    .bind(side)
    .bind(&leg.model)
    .bind(&leg.reasoning_effort)
    .bind(&leg.lineage)
    .bind(&leg.status)
    .bind(leg.duration_ms)
    .bind(leg.loc_added)
    .bind(leg.loc_deleted)
    .bind(leg.files_changed)
    .bind(leg.tests_delta)
    .bind(leg.usage_available)
    .bind(leg.nano_aiu)
    .bind(leg.premium_requests)
    .bind(leg.input_tokens)
    .bind(leg.output_tokens)
    .bind(leg.turn_count)
    .bind(leg.subagent_spawns)
    .bind(leg.tool_errors)
    .bind(SqlxJson(&leg.metrics))
    .execute(&mut **tx)
    .await
    .map_err(|err| {
        tracing::error!(?err, side, "failed to insert shadow bout leg");
        ApiError::internal("failed to record shadow bout")
    })?;
    Ok(())
}

pub async fn record_shadow_bout(
    pool: &PgPool,
    user_id: Uuid,
    mut input: ShadowBoutInput,
) -> Result<RecordedShadowBoutOutput, ApiError> {
    validate_and_normalize_shadow_bout_input(&mut input)?;
    let occurred_at = input.occurred_at.unwrap_or_else(Utc::now);

    let mut tx = pool.begin().await.map_err(|err| {
        tracing::error!(?err, "failed to begin shadow bout transaction");
        ApiError::internal("database error")
    })?;
    lock_user_row(&mut tx, user_id).await?;

    let existing = sqlx::query_as::<_, ExistingBoutRow>(
        "SELECT id, winner, judge_model, judge_lineage, judge_rationale, judged_at, client_sent_at
         FROM arena_shadow_bouts
         WHERE user_id = $1 AND bout_key = $2
         FOR UPDATE",
    )
    .bind(user_id)
    .bind(&input.bout_key)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|err| {
        tracing::error!(?err, "failed to look up existing shadow bout");
        ApiError::internal("failed to record shadow bout")
    })?;

    let (bout_id, effective_winner) = match existing {
        Some(prev) => {
            let incoming_is_stale = match (input.client_sent_at, prev.client_sent_at) {
                (Some(inc), Some(old)) => inc < old,
                _ => false,
            };

            if incoming_is_stale {
                // Never clobber newer execution data, but still accept a missing judgment.
                if prev.winner.is_empty() && !input.winner.is_empty() {
                    sqlx::query(
                        "UPDATE arena_shadow_bouts
                         SET winner = $2, judge_model = $3, judge_lineage = $4,
                             judge_rationale = $5, judged_at = $6, recorded_at = NOW()
                         WHERE id = $1",
                    )
                    .bind(prev.id)
                    .bind(&input.winner)
                    .bind(&input.judge_model)
                    .bind(&input.judge_lineage)
                    .bind(&input.judge_rationale)
                    .bind(input.judged_at)
                    .execute(&mut *tx)
                    .await
                    .map_err(|err| {
                        tracing::error!(?err, "failed to backfill shadow bout judgment");
                        ApiError::internal("failed to record shadow bout")
                    })?;
                    (prev.id, input.winner.clone())
                } else {
                    (prev.id, prev.winner)
                }
            } else {
                // Never overwrite a recorded judgment with an empty winner on replay.
                let (winner, judge_model, judge_lineage, judge_rationale, judged_at) =
                    if input.winner.is_empty() && !prev.winner.is_empty() {
                        (
                            prev.winner,
                            prev.judge_model,
                            prev.judge_lineage,
                            prev.judge_rationale,
                            prev.judged_at,
                        )
                    } else {
                        (
                            input.winner.clone(),
                            input.judge_model.clone(),
                            input.judge_lineage.clone(),
                            input.judge_rationale.clone(),
                            input.judged_at,
                        )
                    };

                sqlx::query(
                    "UPDATE arena_shadow_bouts
                     SET track = $2,
                         champion_source = $3,
                         champion_human_turns = $4,
                         repo = $5,
                         base_sha = $6,
                         task_ref = $7,
                         task_kind = $8,
                         prompt_sha256 = $9,
                         champion_impl_champion_test = $10,
                         challenger_impl_challenger_test = $11,
                         champion_impl_challenger_test = $12,
                         challenger_impl_champion_test = $13,
                         winner = $14,
                         judge_model = $15,
                         judge_lineage = $16,
                         judge_rationale = $17,
                         judged_at = $18,
                         occurred_at = $19,
                         recorded_at = NOW(),
                         client_sent_at = COALESCE($20, client_sent_at)
                     WHERE id = $1",
                )
                .bind(prev.id)
                .bind(&input.track)
                .bind(&input.champion_source)
                .bind(input.champion_human_turns)
                .bind(&input.repo)
                .bind(&input.base_sha)
                .bind(&input.task_ref)
                .bind(&input.task_kind)
                .bind(&input.prompt_sha256)
                .bind(&input.matrix.champion_impl_champion_test)
                .bind(&input.matrix.challenger_impl_challenger_test)
                .bind(&input.matrix.champion_impl_challenger_test)
                .bind(&input.matrix.challenger_impl_champion_test)
                .bind(&winner)
                .bind(&judge_model)
                .bind(&judge_lineage)
                .bind(&judge_rationale)
                .bind(judged_at)
                .bind(occurred_at)
                .bind(input.client_sent_at)
                .execute(&mut *tx)
                .await
                .map_err(|err| {
                    tracing::error!(?err, "failed to update shadow bout");
                    ApiError::internal("failed to record shadow bout")
                })?;

                sqlx::query("DELETE FROM arena_shadow_bout_legs WHERE bout_id = $1")
                    .bind(prev.id)
                    .execute(&mut *tx)
                    .await
                    .map_err(|err| {
                        tracing::error!(?err, "failed to clear shadow bout legs");
                        ApiError::internal("failed to record shadow bout")
                    })?;

                insert_leg(&mut tx, prev.id, "champion", &input.champion).await?;
                insert_leg(&mut tx, prev.id, "challenger", &input.challenger).await?;
                (prev.id, winner)
            }
        }
        None => {
            let new_id: i64 = sqlx::query_scalar(
                "INSERT INTO arena_shadow_bouts (
                    user_id, bout_key, track, champion_source, champion_human_turns,
                    repo, base_sha, task_ref, task_kind, prompt_sha256,
                    champion_impl_champion_test, challenger_impl_challenger_test,
                    champion_impl_challenger_test, challenger_impl_champion_test,
                    winner, judge_model, judge_lineage, judge_rationale, judged_at,
                    occurred_at, client_sent_at
                 )
                 VALUES (
                    $1, $2, $3, $4, $5,
                    $6, $7, $8, $9, $10,
                    $11, $12, $13, $14,
                    $15, $16, $17, $18, $19,
                    $20, $21
                 )
                 RETURNING id",
            )
            .bind(user_id)
            .bind(&input.bout_key)
            .bind(&input.track)
            .bind(&input.champion_source)
            .bind(input.champion_human_turns)
            .bind(&input.repo)
            .bind(&input.base_sha)
            .bind(&input.task_ref)
            .bind(&input.task_kind)
            .bind(&input.prompt_sha256)
            .bind(&input.matrix.champion_impl_champion_test)
            .bind(&input.matrix.challenger_impl_challenger_test)
            .bind(&input.matrix.champion_impl_challenger_test)
            .bind(&input.matrix.challenger_impl_champion_test)
            .bind(&input.winner)
            .bind(&input.judge_model)
            .bind(&input.judge_lineage)
            .bind(&input.judge_rationale)
            .bind(input.judged_at)
            .bind(occurred_at)
            .bind(input.client_sent_at)
            .fetch_one(&mut *tx)
            .await
            .map_err(|err| {
                tracing::error!(?err, "failed to insert shadow bout");
                ApiError::internal("failed to record shadow bout")
            })?;

            insert_leg(&mut tx, new_id, "champion", &input.champion).await?;
            insert_leg(&mut tx, new_id, "challenger", &input.challenger).await?;
            (new_id, input.winner.clone())
        }
    };

    tx.commit().await.map_err(|err| {
        tracing::error!(?err, "failed to commit shadow bout");
        ApiError::internal("failed to record shadow bout")
    })?;

    Ok(RecordedShadowBoutOutput {
        bout_id,
        bout_key: input.bout_key,
        winner: effective_winner,
        spooled: false,
    })
}

#[derive(Clone, sqlx::FromRow)]
struct BoutRow {
    id: i64,
    bout_key: String,
    track: String,
    champion_source: String,
    champion_human_turns: i32,
    repo: String,
    base_sha: String,
    task_ref: String,
    task_kind: String,
    champion_impl_champion_test: String,
    challenger_impl_challenger_test: String,
    champion_impl_challenger_test: String,
    challenger_impl_champion_test: String,
    winner: String,
    judge_model: String,
    judge_lineage: String,
    judge_rationale: String,
    judged_at: Option<DateTime<Utc>>,
    occurred_at: DateTime<Utc>,
}

#[derive(Clone, sqlx::FromRow)]
struct BoutLegRow {
    bout_id: i64,
    side: String,
    model: String,
    reasoning_effort: String,
    lineage: String,
    status: String,
    duration_ms: i64,
    loc_added: i32,
    loc_deleted: i32,
    files_changed: i32,
    tests_delta: i32,
    usage_available: bool,
    nano_aiu: i64,
    premium_requests: f64,
    input_tokens: i64,
    output_tokens: i64,
    turn_count: i32,
    subagent_spawns: i32,
    tool_errors: i32,
}

impl From<BoutLegRow> for ShadowBoutSideView {
    fn from(r: BoutLegRow) -> Self {
        Self {
            side: r.side,
            model: r.model,
            reasoning_effort: r.reasoning_effort,
            lineage: r.lineage,
            status: r.status,
            duration_ms: r.duration_ms,
            loc_added: r.loc_added,
            loc_deleted: r.loc_deleted,
            files_changed: r.files_changed,
            tests_delta: r.tests_delta,
            usage_available: r.usage_available,
            nano_aiu: r.nano_aiu,
            credits: round4(r.nano_aiu as f64 / 1_000_000_000.0),
            premium_requests: r.premium_requests,
            input_tokens: r.input_tokens,
            output_tokens: r.output_tokens,
            turn_count: r.turn_count,
            subagent_spawns: r.subagent_spawns,
            tool_errors: r.tool_errors,
        }
    }
}

fn round4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

fn median_f64(mut vals: Vec<f64>) -> f64 {
    if vals.is_empty() {
        return 0.0;
    }
    vals.sort_by(|a, b| a.total_cmp(b));
    let mid = vals.len() / 2;
    if vals.len() % 2 == 1 {
        vals[mid]
    } else {
        (vals[mid - 1] + vals[mid]) / 2.0
    }
}

fn median_i64(mut vals: Vec<i64>) -> i64 {
    if vals.is_empty() {
        return 0;
    }
    vals.sort_unstable();
    let mid = vals.len() / 2;
    if vals.len() % 2 == 1 {
        vals[mid]
    } else {
        (vals[mid - 1] + vals[mid]) / 2
    }
}

#[derive(Default)]
struct ModelAccum {
    track: String,
    model: String,
    reasoning_effort: String,
    lineage: String,
    bouts: i32,
    wins: i32,
    losses: i32,
    ties: i32,
    both_failed: i32,
    own_test_pass: i32,
    own_test_evaluated: i32,
    credits: Vec<f64>,
    durations_ms: Vec<i64>,
    loc_churn_sum: i64,
    turns_sum: i64,
    subagents_sum: i64,
    tool_errors_sum: i64,
}

fn aggregate_shadow_bouts(bouts: &[ShadowBoutView], window_days: i32) -> ShadowLeaderboard {
    let mut models_map: HashMap<(String, String, String), ModelAccum> = HashMap::new();
    let mut pairs_map: HashMap<(String, String, String, String, String), ShadowPairwiseRecord> =
        HashMap::new();
    let mut judged_total = 0;

    for bout in bouts {
        if !bout.winner.is_empty() {
            judged_total += 1;
        }

        let sides = [
            (
                &bout.champion,
                &bout.matrix.champion_impl_champion_test,
                "champion",
            ),
            (
                &bout.challenger,
                &bout.matrix.challenger_impl_challenger_test,
                "challenger",
            ),
        ];

        for (side_view, own_test_cell, side_role) in sides {
            let (m_key, e_key) = model_effort_key(&side_view.model, &side_view.reasoning_effort);
            let acc = models_map
                .entry((bout.track.clone(), m_key, e_key))
                .or_insert_with(|| ModelAccum {
                    track: bout.track.clone(),
                    model: side_view.model.clone(),
                    reasoning_effort: side_view.reasoning_effort.clone(),
                    lineage: side_view.lineage.clone(),
                    ..Default::default()
                });
            acc.bouts += 1;
            match bout.winner.as_str() {
                w if w == side_role => acc.wins += 1,
                "champion" | "challenger" => acc.losses += 1,
                "tie" => acc.ties += 1,
                "both_failed" => acc.both_failed += 1,
                _ => {}
            }
            if !matches!(own_test_cell.as_str(), "skipped" | "no_tests") {
                acc.own_test_evaluated += 1;
                if own_test_cell == "pass" {
                    acc.own_test_pass += 1;
                }
            }
            if side_view.usage_available {
                acc.credits
                    .push(side_view.nano_aiu as f64 / 1_000_000_000.0);
            }
            if side_view.duration_ms > 0 {
                acc.durations_ms.push(side_view.duration_ms);
            }
            acc.loc_churn_sum += (side_view.loc_added + side_view.loc_deleted) as i64;
            acc.turns_sum += side_view.turn_count as i64;
            acc.subagents_sum += side_view.subagent_spawns as i64;
            acc.tool_errors_sum += side_view.tool_errors as i64;
        }

        // Canonical head-to-head pair ordering so (A vs B) and (B vs A) merge cleanly.
        let k_champ = model_effort_key(&bout.champion.model, &bout.champion.reasoning_effort);
        let k_chall = model_effort_key(&bout.challenger.model, &bout.challenger.reasoning_effort);
        let champ_is_a = k_champ <= k_chall;
        let (a_view, b_view, ka, kb) = if champ_is_a {
            (&bout.champion, &bout.challenger, k_champ, k_chall)
        } else {
            (&bout.challenger, &bout.champion, k_chall, k_champ)
        };
        let pair = pairs_map
            .entry((bout.track.clone(), ka.0, ka.1, kb.0, kb.1))
            .or_insert_with(|| ShadowPairwiseRecord {
                track: bout.track.clone(),
                model_a: a_view.model.clone(),
                effort_a: a_view.reasoning_effort.clone(),
                model_b: b_view.model.clone(),
                effort_b: b_view.reasoning_effort.clone(),
                ..Default::default()
            });
        pair.bouts += 1;
        match bout.winner.as_str() {
            "champion" => {
                if champ_is_a {
                    pair.a_wins += 1;
                } else {
                    pair.b_wins += 1;
                }
            }
            "challenger" => {
                if champ_is_a {
                    pair.b_wins += 1;
                } else {
                    pair.a_wins += 1;
                }
            }
            "tie" => pair.ties += 1,
            "both_failed" => pair.both_failed += 1,
            _ => {}
        }
    }

    let mut models: Vec<ShadowModelScorecard> = models_map
        .into_values()
        .map(|a| {
            let judged = a.wins + a.losses + a.ties + a.both_failed;
            let decided = a.wins + a.losses + a.ties;
            let win_rate = if decided > 0 {
                round4((a.wins as f64 + 0.5 * a.ties as f64) / decided as f64)
            } else {
                0.0
            };
            let own_test_pass_rate = if a.own_test_evaluated > 0 {
                round4(a.own_test_pass as f64 / a.own_test_evaluated as f64)
            } else {
                0.0
            };
            let denom = a.bouts.max(1) as f64;
            ShadowModelScorecard {
                track: a.track,
                model: a.model,
                reasoning_effort: a.reasoning_effort,
                lineage: a.lineage,
                bouts: a.bouts,
                judged_bouts: judged,
                wins: a.wins,
                losses: a.losses,
                ties: a.ties,
                both_failed: a.both_failed,
                win_rate,
                own_test_pass_rate,
                median_credits: round4(median_f64(a.credits)),
                p50_duration_ms: median_i64(a.durations_ms),
                avg_loc_churn: (a.loc_churn_sum as f64 / denom).round() as i32,
                avg_turns: round4(a.turns_sum as f64 / denom),
                avg_subagent_spawns: round4(a.subagents_sum as f64 / denom),
                avg_tool_errors: round4(a.tool_errors_sum as f64 / denom),
            }
        })
        .collect();

    models.sort_by(|a, b| {
        a.track
            .cmp(&b.track)
            .then_with(|| b.win_rate.total_cmp(&a.win_rate))
            .then_with(|| b.judged_bouts.cmp(&a.judged_bouts))
            .then_with(|| b.bouts.cmp(&a.bouts))
            .then_with(|| a.model.cmp(&b.model))
    });

    let mut pairs: Vec<ShadowPairwiseRecord> = pairs_map.into_values().collect();
    pairs.sort_by(|a, b| {
        a.track
            .cmp(&b.track)
            .then_with(|| b.bouts.cmp(&a.bouts))
            .then_with(|| a.model_a.cmp(&b.model_a))
            .then_with(|| a.model_b.cmp(&b.model_b))
    });

    ShadowLeaderboard {
        window_days,
        total_bouts: bouts.len() as i32,
        judged_bouts: judged_total,
        models,
        pairs,
        recent_bouts: bouts.iter().take(30).cloned().collect(),
    }
}

pub async fn get_shadow_leaderboard(
    pool: &PgPool,
    user_id: Uuid,
    query: ShadowLeaderboardQuery,
) -> Result<ShadowLeaderboard, ApiError> {
    let days = query.days.unwrap_or(90);
    if !(1..=3650).contains(&days) {
        return Err(ApiError::bad_request("days must be between 1 and 3650"));
    }

    let track_filter = query
        .track
        .as_deref()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty());
    if let Some(t) = &track_filter
        && !VALID_SHADOW_TRACKS.contains(&t.as_str())
    {
        return Err(ApiError::bad_request(format!(
            "track must be one of {:?}",
            VALID_SHADOW_TRACKS
        )));
    }

    let source_filter = query
        .champion_source
        .as_deref()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty());
    if let Some(s) = &source_filter
        && !VALID_CHAMPION_SOURCES.contains(&s.as_str())
    {
        return Err(ApiError::bad_request(format!(
            "championSource must be one of {:?}",
            VALID_CHAMPION_SOURCES
        )));
    }

    let repo_filter = query
        .repo
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let since = query
        .from
        .unwrap_or_else(|| Utc::now() - ChronoDuration::days(days as i64));
    let until = query.to;

    let bout_rows = sqlx::query_as::<_, BoutRow>(
        "SELECT id, bout_key, track, champion_source, champion_human_turns,
                repo, base_sha, task_ref, task_kind,
                champion_impl_champion_test, challenger_impl_challenger_test,
                champion_impl_challenger_test, challenger_impl_champion_test,
                winner, judge_model, judge_lineage, judge_rationale, judged_at, occurred_at
         FROM arena_shadow_bouts
         WHERE user_id = $1
           AND occurred_at >= $2
           AND ($3::timestamptz IS NULL OR occurred_at <= $3)
           AND ($4::text IS NULL OR track = $4)
           AND ($5::text IS NULL OR champion_source = $5)
           AND ($6::text IS NULL OR repo = $6)
         ORDER BY occurred_at DESC, id DESC",
    )
    .bind(user_id)
    .bind(since)
    .bind(until)
    .bind(&track_filter)
    .bind(&source_filter)
    .bind(&repo_filter)
    .fetch_all(pool)
    .await
    .map_err(|err| {
        tracing::error!(?err, "failed to query shadow bouts");
        ApiError::internal("failed to load shadow leaderboard")
    })?;

    if bout_rows.is_empty() {
        return Ok(ShadowLeaderboard {
            window_days: days,
            total_bouts: 0,
            judged_bouts: 0,
            models: Vec::new(),
            pairs: Vec::new(),
            recent_bouts: Vec::new(),
        });
    }

    let bout_ids: Vec<i64> = bout_rows.iter().map(|b| b.id).collect();
    let leg_rows = sqlx::query_as::<_, BoutLegRow>(
        "SELECT bout_id, side, model, reasoning_effort, lineage, status,
                duration_ms, loc_added, loc_deleted, files_changed, tests_delta,
                usage_available, nano_aiu, premium_requests, input_tokens, output_tokens,
                turn_count, subagent_spawns, tool_errors
         FROM arena_shadow_bout_legs
         WHERE bout_id = ANY($1)",
    )
    .bind(&bout_ids)
    .fetch_all(pool)
    .await
    .map_err(|err| {
        tracing::error!(?err, "failed to query shadow bout legs");
        ApiError::internal("failed to load shadow leaderboard")
    })?;

    let mut legs_by_bout: HashMap<i64, (Option<ShadowBoutSideView>, Option<ShadowBoutSideView>)> =
        HashMap::with_capacity(bout_ids.len());
    for leg in leg_rows {
        let entry = legs_by_bout.entry(leg.bout_id).or_default();
        if leg.side == "champion" {
            entry.0 = Some(leg.into());
        } else if leg.side == "challenger" {
            entry.1 = Some(leg.into());
        }
    }

    let mut views = Vec::with_capacity(bout_rows.len());
    for b in bout_rows {
        let Some((Some(champion), Some(challenger))) = legs_by_bout.remove(&b.id) else {
            continue;
        };
        views.push(ShadowBoutView {
            id: b.id,
            bout_key: b.bout_key,
            track: b.track,
            champion_source: b.champion_source,
            champion_human_turns: b.champion_human_turns,
            repo: b.repo,
            base_sha: b.base_sha,
            task_ref: b.task_ref,
            task_kind: b.task_kind,
            matrix: ShadowBoutMatrixInput {
                champion_impl_champion_test: b.champion_impl_champion_test,
                challenger_impl_challenger_test: b.challenger_impl_challenger_test,
                champion_impl_challenger_test: b.champion_impl_challenger_test,
                challenger_impl_champion_test: b.challenger_impl_champion_test,
            },
            winner: b.winner,
            judge_model: b.judge_model,
            judge_lineage: b.judge_lineage,
            judge_rationale: b.judge_rationale,
            judged_at: b.judged_at,
            occurred_at: b.occurred_at,
            champion,
            challenger,
        });
    }

    Ok(aggregate_shadow_bouts(&views, days))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_side(model: &str, nano_aiu: i64, duration_ms: i64) -> ShadowBoutSideInput {
        ShadowBoutSideInput {
            model: model.to_string(),
            reasoning_effort: "high".to_string(),
            status: "ok".to_string(),
            duration_ms,
            loc_added: 40,
            loc_deleted: 10,
            files_changed: 2,
            tests_delta: 2,
            usage_available: true,
            nano_aiu,
            premium_requests: 3.0,
            input_tokens: 10_000,
            output_tokens: 1_500,
            turn_count: 4,
            subagent_spawns: 2,
            tool_errors: 0,
            metrics: serde_json::json!({"aiCreditsStatus": "complete"}),
            ..Default::default()
        }
    }

    fn sample_bout(key: &str, winner: &str) -> ShadowBoutInput {
        ShadowBoutInput {
            bout_key: key.to_string(),
            track: "coding".to_string(),
            champion_source: "headless".to_string(),
            champion_human_turns: 0,
            repo: "robert-crandall/demo".to_string(),
            base_sha: "abc123".to_string(),
            task_ref: "PR #1".to_string(),
            task_kind: "feature".to_string(),
            prompt_sha256: "deadbeef".to_string(),
            champion: sample_side("claude-opus-5.5", 4_000_000_000, 30_000),
            challenger: sample_side("gpt-6.2-sol", 2_500_000_000, 20_000),
            matrix: ShadowBoutMatrixInput {
                champion_impl_champion_test: "pass".to_string(),
                challenger_impl_challenger_test: "pass".to_string(),
                champion_impl_challenger_test: "skipped".to_string(),
                challenger_impl_champion_test: "skipped".to_string(),
            },
            winner: winner.to_string(),
            judge_model: "gemini-4-carbon".to_string(),
            judge_lineage: String::new(),
            judge_rationale: "cleaner diff".to_string(),
            judged_at: None,
            occurred_at: None,
            client_sent_at: None,
            queue_only: false,
        }
    }

    #[test]
    fn validation_normalizes_lineages_and_rejects_bad_values() {
        let mut bout = sample_bout("sb_1", "challenger");
        validate_and_normalize_shadow_bout_input(&mut bout).expect("valid bout");
        assert_eq!(bout.champion.lineage, "anthropic");
        assert_eq!(bout.challenger.lineage, "openai");
        assert_eq!(bout.judge_lineage, "google");
        assert!(bout.judged_at.is_some());

        let mut bad_track = sample_bout("sb_2", "");
        bad_track.track = "review".to_string();
        assert!(validate_and_normalize_shadow_bout_input(&mut bad_track).is_err());

        let mut bad_cell = sample_bout("sb_3", "");
        bad_cell.matrix.champion_impl_champion_test = "bogus".to_string();
        assert!(validate_and_normalize_shadow_bout_input(&mut bad_cell).is_err());

        let mut bad_metrics = sample_bout("sb_4", "");
        bad_metrics.champion.metrics = serde_json::json!(["not-an-object"]);
        assert!(validate_and_normalize_shadow_bout_input(&mut bad_metrics).is_err());
    }

    #[test]
    fn aggregate_merges_symmetric_pairs_and_computes_win_rate_and_credits() {
        let now = Utc::now();
        let make_view =
            |id, key: &str, champ: &str, chall: &str, winner: &str, c_aiu, ch_aiu| ShadowBoutView {
                id,
                bout_key: key.to_string(),
                track: "coding".to_string(),
                champion_source: "headless".to_string(),
                champion_human_turns: 0,
                repo: "r/demo".to_string(),
                base_sha: "aaa".to_string(),
                task_ref: "#1".to_string(),
                task_kind: "feature".to_string(),
                matrix: ShadowBoutMatrixInput {
                    champion_impl_champion_test: "pass".to_string(),
                    challenger_impl_challenger_test: "fail".to_string(),
                    champion_impl_challenger_test: "skipped".to_string(),
                    challenger_impl_champion_test: "skipped".to_string(),
                },
                winner: winner.to_string(),
                judge_model: "gemini-4-carbon".to_string(),
                judge_lineage: "google".to_string(),
                judge_rationale: String::new(),
                judged_at: if winner.is_empty() { None } else { Some(now) },
                occurred_at: now,
                champion: ShadowBoutSideView {
                    side: "champion".to_string(),
                    model: champ.to_string(),
                    reasoning_effort: "high".to_string(),
                    lineage: infer_lineage(champ, &[]),
                    status: "ok".to_string(),
                    duration_ms: 40_000,
                    loc_added: 60,
                    loc_deleted: 10,
                    files_changed: 2,
                    tests_delta: 1,
                    usage_available: true,
                    nano_aiu: c_aiu,
                    credits: c_aiu as f64 / 1e9,
                    premium_requests: 2.0,
                    input_tokens: 1000,
                    output_tokens: 200,
                    turn_count: 3,
                    subagent_spawns: 1,
                    tool_errors: 0,
                },
                challenger: ShadowBoutSideView {
                    side: "challenger".to_string(),
                    model: chall.to_string(),
                    reasoning_effort: "high".to_string(),
                    lineage: infer_lineage(chall, &[]),
                    status: "ok".to_string(),
                    duration_ms: 20_000,
                    loc_added: 30,
                    loc_deleted: 5,
                    files_changed: 1,
                    tests_delta: 1,
                    usage_available: true,
                    nano_aiu: ch_aiu,
                    credits: ch_aiu as f64 / 1e9,
                    premium_requests: 1.0,
                    input_tokens: 800,
                    output_tokens: 150,
                    turn_count: 2,
                    subagent_spawns: 0,
                    tool_errors: 1,
                },
            };

        // Bout 1: opus (champion) beats sol (challenger).
        // Bout 2: sol (champion) ties opus (challenger) - seats reversed!
        let bouts = vec![
            make_view(
                1,
                "b1",
                "claude-opus-5.5",
                "gpt-6.2-sol",
                "champion",
                4_000_000_000,
                2_000_000_000,
            ),
            make_view(
                2,
                "b2",
                "gpt-6.2-sol",
                "claude-opus-5.5",
                "tie",
                3_000_000_000,
                5_000_000_000,
            ),
        ];

        let lb = aggregate_shadow_bouts(&bouts, 90);
        assert_eq!(lb.total_bouts, 2);
        assert_eq!(lb.judged_bouts, 2);
        assert_eq!(lb.pairs.len(), 1);
        assert_eq!(lb.pairs[0].model_a, "claude-opus-5.5");
        assert_eq!(lb.pairs[0].model_b, "gpt-6.2-sol");
        assert_eq!(lb.pairs[0].a_wins, 1);
        assert_eq!(lb.pairs[0].b_wins, 0);
        assert_eq!(lb.pairs[0].ties, 1);

        let opus = lb
            .models
            .iter()
            .find(|m| m.model == "claude-opus-5.5")
            .unwrap();
        assert_eq!(opus.wins, 1);
        assert_eq!(opus.losses, 0);
        assert_eq!(opus.ties, 1);
        assert!((opus.win_rate - 0.75).abs() < 1e-6);
        assert!((opus.median_credits - 4.5).abs() < 1e-6);
        assert!((opus.own_test_pass_rate - 0.5).abs() < 1e-6);
    }
}
