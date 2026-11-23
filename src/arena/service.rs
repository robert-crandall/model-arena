use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::arena::types::{
    CodingRunInput, DecideTrialInput, OverrideCandidateInput, OverrideCandidateResponse,
    OverrideCodingRunInput, OverrideCodingRunResponse, PanelLeg, PanelSelection, RecordRunInput,
    RecordedCodingRunOutput, RecordedRunOutput, RosterEntry, RosterSlotInput,
    SKILL_ADVERSARIAL_REVIEW, SKILL_PLAN_TOURNAMENT, SelectPanelInput, StartTrialInput,
    TOURNAMENT_LENSES, Trial, VALID_APPROACH_VERDICTS, VALID_CANDIDATE_DISPOSITIONS,
    VALID_CANDIDATE_SEVERITIES, VALID_LEG_ROLES, VALID_LEG_STATUSES, VALID_PHASES,
    VALID_TASK_KINDS, VALID_WINNING_ROLES, classify_size_bucket, derive_candidate_keys,
    infer_lineage, is_trial_competing_role, is_valid_artifact_kind, is_valid_skill,
    model_effort_key, normalize_reasoning_effort,
};
use crate::auth::random_hex_secret;
use crate::routes::ApiError;

/// Serializes per-user arena mutations without blocking `FOR KEY SHARE` foreign-key checks on child inserts.
pub(crate) async fn lock_user_row(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<(), ApiError> {
    let exists: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM users WHERE id = $1 FOR NO KEY UPDATE")
            .bind(user_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|err| {
                tracing::error!(?err, "failed to lock user row");
                ApiError::internal("database error")
            })?;

    if exists.is_none() {
        return Err(ApiError::not_found("user not found"));
    }
    Ok(())
}

pub async fn list_roster(
    pool: &PgPool,
    user_id: Uuid,
    skill: Option<&str>,
) -> Result<Vec<RosterEntry>, ApiError> {
    let skill_filter = skill.map(str::trim).unwrap_or("");
    if !skill_filter.is_empty() && !is_valid_skill(skill_filter) {
        return Err(ApiError::bad_request(format!(
            "skill must be {SKILL_ADVERSARIAL_REVIEW} or {SKILL_PLAN_TOURNAMENT}"
        )));
    }

    sqlx::query_as::<_, RosterEntry>(
        "SELECT skill, lineage, champion_model, champion_reasoning_effort,
                backup_model, backup_reasoning_effort, sort_order, enabled, updated_at
         FROM arena_roster
         WHERE user_id = $1 AND ($2 = '' OR skill = $2)
         ORDER BY skill ASC, sort_order ASC, lineage ASC",
    )
    .bind(user_id)
    .bind(skill_filter)
    .fetch_all(pool)
    .await
    .map_err(|err| {
        tracing::error!(?err, "failed to query arena roster");
        ApiError::internal("failed to load model roster")
    })
}

#[derive(sqlx::FromRow)]
struct OpenTrialSlotRow {
    id: i64,
    lineage: String,
    champion_model: String,
    champion_reasoning_effort: String,
}

pub async fn replace_roster(
    pool: &PgPool,
    user_id: Uuid,
    skill: &str,
    mut slots: Vec<RosterSlotInput>,
) -> Result<Vec<RosterEntry>, ApiError> {
    let skill = skill.trim();
    if !is_valid_skill(skill) {
        return Err(ApiError::bad_request(format!(
            "skill must be {SKILL_ADVERSARIAL_REVIEW} or {SKILL_PLAN_TOURNAMENT}"
        )));
    }

    let mut slot_by_lineage = HashMap::with_capacity(slots.len());
    for slot in &mut slots {
        slot.lineage = slot.lineage.trim().to_ascii_lowercase();
        slot.champion_model = slot.champion_model.trim().to_string();
        slot.champion_reasoning_effort =
            normalize_reasoning_effort(&slot.champion_reasoning_effort)
                .map_err(ApiError::bad_request)?;
        slot.backup_model = slot.backup_model.trim().to_string();
        slot.backup_reasoning_effort = normalize_reasoning_effort(&slot.backup_reasoning_effort)
            .map_err(ApiError::bad_request)?;
        if slot.lineage.is_empty() || slot.champion_model.is_empty() {
            return Err(ApiError::bad_request(
                "lineage and championModel must be non-empty",
            ));
        }
        if slot_by_lineage
            .insert(slot.lineage.clone(), slot.clone())
            .is_some()
        {
            return Err(ApiError::bad_request(format!(
                "duplicate lineage {:?} in roster",
                slot.lineage
            )));
        }
    }

    let mut tx = pool.begin().await.map_err(|err| {
        tracing::error!(?err, "failed to begin roster transaction");
        ApiError::internal("database error")
    })?;

    lock_user_row(&mut tx, user_id).await?;

    let open_trials = sqlx::query_as::<_, OpenTrialSlotRow>(
        "SELECT id, lineage, champion_model, champion_reasoning_effort
         FROM arena_trials
         WHERE user_id = $1 AND skill = $2 AND status IN ('active', 'completed')
         FOR UPDATE",
    )
    .bind(user_id)
    .bind(skill)
    .fetch_all(&mut *tx)
    .await
    .map_err(|err| {
        tracing::error!(?err, "failed to check open trials during roster update");
        ApiError::internal("database error")
    })?;

    for trial in open_trials {
        let Some(next_slot) = slot_by_lineage.get(&trial.lineage) else {
            return Err(ApiError::conflict(format!(
                "open trial #{} exists for {} / {} (champion {}); promote or retire it before removing that slot",
                trial.id, skill, trial.lineage, trial.champion_model
            )));
        };
        if !next_slot.enabled
            || model_effort_key(
                &next_slot.champion_model,
                &next_slot.champion_reasoning_effort,
            ) != model_effort_key(&trial.champion_model, &trial.champion_reasoning_effort)
        {
            return Err(ApiError::conflict(format!(
                "open trial #{} exists for {} / {} (champion {}); promote or retire it before changing or disabling that slot",
                trial.id, skill, trial.lineage, trial.champion_model
            )));
        }
    }

    sqlx::query("DELETE FROM arena_roster WHERE user_id = $1 AND skill = $2")
        .bind(user_id)
        .bind(skill)
        .execute(&mut *tx)
        .await
        .map_err(|err| {
            tracing::error!(?err, "failed to clear skill roster");
            ApiError::internal("failed to update roster")
        })?;

    for slot in &slots {
        sqlx::query(
            "INSERT INTO arena_roster (
                user_id, skill, lineage, champion_model, champion_reasoning_effort,
                backup_model, backup_reasoning_effort, sort_order, enabled, updated_at
             )
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, NOW())",
        )
        .bind(user_id)
        .bind(skill)
        .bind(&slot.lineage)
        .bind(&slot.champion_model)
        .bind(&slot.champion_reasoning_effort)
        .bind(&slot.backup_model)
        .bind(&slot.backup_reasoning_effort)
        .bind(slot.sort_order)
        .bind(slot.enabled)
        .execute(&mut *tx)
        .await
        .map_err(|err| {
            tracing::error!(?err, "failed to insert roster slot");
            ApiError::internal("failed to update roster")
        })?;
    }

    tx.commit().await.map_err(|err| {
        tracing::error!(?err, "failed to commit roster update");
        ApiError::internal("failed to update roster")
    })?;

    list_roster(pool, user_id, Some(skill)).await
}

#[derive(sqlx::FromRow)]
struct ChampionSlotRow {
    champion_model: String,
    champion_reasoning_effort: String,
    enabled: bool,
}

#[derive(sqlx::FromRow, Clone)]
pub(crate) struct TrialRow {
    pub id: i64,
    pub skill: String,
    pub lineage: String,
    pub champion_model: String,
    pub champion_reasoning_effort: String,
    pub challenger_model: String,
    pub challenger_reasoning_effort: String,
    pub target_runs: i32,
    pub completed_runs: i32,
    pub swap_percent: i32,
    pub allow_pr_diff: bool,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub decided_at: Option<DateTime<Utc>>,
}

impl From<TrialRow> for Trial {
    fn from(r: TrialRow) -> Self {
        Self {
            id: r.id,
            skill: r.skill,
            lineage: r.lineage,
            champion_model: r.champion_model,
            champion_reasoning_effort: r.champion_reasoning_effort,
            challenger_model: r.challenger_model,
            challenger_reasoning_effort: r.challenger_reasoning_effort,
            target_runs: r.target_runs,
            completed_runs: r.completed_runs,
            swap_percent: r.swap_percent,
            allow_pr_diff: r.allow_pr_diff,
            status: r.status,
            created_at: r.created_at,
            decided_at: r.decided_at,
        }
    }
}

pub async fn start_trial(
    pool: &PgPool,
    user_id: Uuid,
    input: StartTrialInput,
) -> Result<Trial, ApiError> {
    let skill = input.skill.trim();
    if !is_valid_skill(skill) {
        return Err(ApiError::bad_request(format!("invalid skill {skill:?}")));
    }
    let lineage = input.lineage.trim().to_ascii_lowercase();
    let challenger = input.challenger_model.trim();
    if lineage.is_empty() || challenger.is_empty() {
        return Err(ApiError::bad_request(
            "lineage and challengerModel are required",
        ));
    }
    let challenger_effort = normalize_reasoning_effort(&input.challenger_reasoning_effort)
        .map_err(ApiError::bad_request)?;
    if !(1..=500).contains(&input.target_runs) {
        return Err(ApiError::bad_request(
            "targetRuns must be between 1 and 500",
        ));
    }
    let swap_pct = input.swap_percent.unwrap_or(50);
    if !(1..=100).contains(&swap_pct) {
        return Err(ApiError::bad_request(
            "swapPercent must be between 1 and 100",
        ));
    }

    let mut tx = pool.begin().await.map_err(|err| {
        tracing::error!(?err, "failed to begin start_trial transaction");
        ApiError::internal("database error")
    })?;

    lock_user_row(&mut tx, user_id).await?;

    let champ = sqlx::query_as::<_, ChampionSlotRow>(
        "SELECT champion_model, champion_reasoning_effort, enabled
         FROM arena_roster
         WHERE user_id = $1 AND skill = $2 AND lineage = $3",
    )
    .bind(user_id)
    .bind(skill)
    .bind(&lineage)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|err| {
        tracing::error!(?err, "failed to query champion slot");
        ApiError::internal("database error")
    })?
    .ok_or_else(|| {
        ApiError::not_found(format!(
            "no roster entry for skill {skill} lineage {lineage}"
        ))
    })?;

    if !champ.enabled {
        return Err(ApiError::bad_request(format!(
            "roster lineage {lineage} is disabled for {skill}"
        )));
    }
    if model_effort_key(&champ.champion_model, &champ.champion_reasoning_effort)
        == model_effort_key(challenger, &challenger_effort)
    {
        return Err(ApiError::bad_request(format!(
            "challenger {challenger:?} (effort {challenger_effort:?}) is already the champion"
        )));
    }

    let row = sqlx::query_as::<_, TrialRow>(
        "INSERT INTO arena_trials (
            user_id, skill, lineage, champion_model, champion_reasoning_effort,
            challenger_model, challenger_reasoning_effort,
            target_runs, swap_percent, allow_pr_diff
         )
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         RETURNING
            id, skill, lineage, champion_model, champion_reasoning_effort,
            challenger_model, challenger_reasoning_effort,
            target_runs, 0::INT AS completed_runs, swap_percent, allow_pr_diff,
            status, created_at, decided_at",
    )
    .bind(user_id)
    .bind(skill)
    .bind(&lineage)
    .bind(&champ.champion_model)
    .bind(&champ.champion_reasoning_effort)
    .bind(challenger)
    .bind(&challenger_effort)
    .bind(input.target_runs)
    .bind(swap_pct)
    .bind(input.allow_pr_diff)
    .fetch_one(&mut *tx)
    .await
    .map_err(|err| {
        if let sqlx::Error::Database(db_err) = &err
            && db_err.code().as_deref() == Some("23505")
        {
            return ApiError::conflict(format!(
                "an open trial already exists for {skill} / {lineage}"
            ));
        }
        tracing::error!(?err, "failed to insert trial");
        ApiError::internal("failed to start trial")
    })?;

    tx.commit().await.map_err(|err| {
        tracing::error!(?err, "failed to commit trial");
        ApiError::internal("failed to start trial")
    })?;

    Ok(row.into())
}

pub async fn list_trials(
    pool: &PgPool,
    user_id: Uuid,
    status_filter: Option<&str>,
) -> Result<Vec<Trial>, ApiError> {
    let status = status_filter.map(str::trim).unwrap_or("");
    if !matches!(status, "" | "active" | "completed" | "promoted" | "retired") {
        return Err(ApiError::bad_request(format!(
            "invalid trial status filter {status:?}"
        )));
    }

    let rows = sqlx::query_as::<_, TrialRow>(
        "SELECT
            t.id, t.skill, t.lineage, t.champion_model, t.champion_reasoning_effort,
            t.challenger_model, t.challenger_reasoning_effort,
            t.target_runs, t.swap_percent, t.allow_pr_diff, t.status, t.created_at, t.decided_at,
            COALESCE((
                SELECT COUNT(DISTINCT l.run_id)::INT
                FROM arena_run_legs l
                JOIN arena_runs r ON r.id = l.run_id
                WHERE l.trial_id = t.id
                  AND r.user_id = t.user_id
                  AND r.skill = t.skill
                  AND l.lineage = t.lineage
                  AND l.is_challenger = TRUE
                  AND lower(l.model) = lower(t.challenger_model)
                  AND l.reasoning_effort = t.challenger_reasoning_effort
                  AND (
                      (t.skill = 'adversarial-review' AND l.role = 'reviewer')
                      OR (t.skill = 'plan-tournament' AND l.role IN ('minimal_diff', 'clean_architecture', 'edge_cases'))
                  )
                  AND (t.allow_pr_diff = TRUE OR r.artifact_kind <> 'pr_diff')
                  AND r.occurred_at >= t.created_at
                  AND (t.decided_at IS NULL OR r.occurred_at <= t.decided_at)
                  AND l.status IN ('ok', 'retried')
            ), 0) AS completed_runs
         FROM arena_trials t
         WHERE t.user_id = $1 AND ($2 = '' OR t.status = $2)
         ORDER BY
            CASE t.status WHEN 'completed' THEN 0 WHEN 'active' THEN 1 ELSE 2 END,
            t.created_at DESC",
    )
    .bind(user_id)
    .bind(status)
    .fetch_all(pool)
    .await
    .map_err(|err| {
        tracing::error!(?err, "failed to list trials");
        ApiError::internal("failed to load trials")
    })?;

    Ok(rows.into_iter().map(Trial::from).collect())
}

pub(crate) async fn count_trial_challenger_runs(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    trial: &TrialRow,
) -> Result<i32, ApiError> {
    sqlx::query_scalar(
        "SELECT COUNT(DISTINCT l.run_id)::INT
         FROM arena_run_legs l
         JOIN arena_runs r ON r.id = l.run_id
         WHERE l.trial_id = $1
           AND r.user_id = $2
           AND r.skill = $3
           AND l.lineage = $4
           AND l.is_challenger = TRUE
           AND lower(l.model) = lower($5)
           AND l.reasoning_effort = $6
           AND (
               ($3 = 'adversarial-review' AND l.role = 'reviewer')
               OR ($3 = 'plan-tournament' AND l.role IN ('minimal_diff', 'clean_architecture', 'edge_cases'))
           )
           AND ($7 = TRUE OR r.artifact_kind <> 'pr_diff')
           AND r.occurred_at >= $8
           AND ($9::timestamptz IS NULL OR r.occurred_at <= $9)
           AND l.status IN ('ok', 'retried')",
    )
    .bind(trial.id)
    .bind(user_id)
    .bind(&trial.skill)
    .bind(&trial.lineage)
    .bind(&trial.challenger_model)
    .bind(&trial.challenger_reasoning_effort)
    .bind(trial.allow_pr_diff)
    .bind(trial.created_at)
    .bind(trial.decided_at)
    .fetch_one(&mut **tx)
    .await
    .map_err(|err| {
        tracing::error!(?err, "failed to count trial runs");
        ApiError::internal("database error")
    })
}

pub async fn decide_trial(
    pool: &PgPool,
    user_id: Uuid,
    trial_id: i64,
    input: DecideTrialInput,
) -> Result<Trial, ApiError> {
    let decision = input.decision.trim();
    if decision != "promote" && decision != "retire" {
        return Err(ApiError::bad_request("decision must be promote or retire"));
    }

    let mut tx = pool.begin().await.map_err(|err| {
        tracing::error!(?err, "failed to begin decide_trial transaction");
        ApiError::internal("database error")
    })?;

    lock_user_row(&mut tx, user_id).await?;

    let mut trial = sqlx::query_as::<_, TrialRow>(
        "SELECT
            id, skill, lineage, champion_model, champion_reasoning_effort,
            challenger_model, challenger_reasoning_effort,
            target_runs, 0::INT AS completed_runs, swap_percent, allow_pr_diff,
            status, created_at, decided_at
         FROM arena_trials
         WHERE id = $1 AND user_id = $2
         FOR UPDATE",
    )
    .bind(trial_id)
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|err| {
        tracing::error!(?err, "failed to load trial for decision");
        ApiError::internal("database error")
    })?
    .ok_or_else(|| ApiError::not_found(format!("trial {trial_id} not found")))?;

    if trial.status != "active" && trial.status != "completed" {
        return Err(ApiError::conflict(format!(
            "trial {trial_id} is already {}",
            trial.status
        )));
    }

    let next_status = if decision == "promote" {
        let updated = sqlx::query(
            "UPDATE arena_roster
             SET backup_model = champion_model,
                 backup_reasoning_effort = champion_reasoning_effort,
                 champion_model = $4,
                 champion_reasoning_effort = $5,
                 updated_at = NOW()
             WHERE user_id = $1 AND skill = $2 AND lineage = $3
               AND lower(champion_model) = lower($6)
               AND champion_reasoning_effort = $7",
        )
        .bind(user_id)
        .bind(&trial.skill)
        .bind(&trial.lineage)
        .bind(&trial.challenger_model)
        .bind(&trial.challenger_reasoning_effort)
        .bind(&trial.champion_model)
        .bind(&trial.champion_reasoning_effort)
        .execute(&mut *tx)
        .await
        .map_err(|err| {
            tracing::error!(?err, "failed to promote challenger in roster");
            ApiError::internal("failed to update roster")
        })?;

        if updated.rows_affected() == 0 {
            return Err(ApiError::conflict(format!(
                "lineage {:?} incumbent no longer matches trial champion {:?} in {} roster",
                trial.lineage, trial.champion_model, trial.skill
            )));
        }
        "promoted"
    } else {
        "retired"
    };

    let (status, decided_at): (String, Option<DateTime<Utc>>) = sqlx::query_as(
        "UPDATE arena_trials
         SET status = $3, decided_at = NOW()
         WHERE id = $1 AND user_id = $2
         RETURNING status, decided_at",
    )
    .bind(trial_id)
    .bind(user_id)
    .bind(next_status)
    .fetch_one(&mut *tx)
    .await
    .map_err(|err| {
        tracing::error!(?err, "failed to update trial status");
        ApiError::internal("failed to decide trial")
    })?;

    trial.status = status;
    trial.decided_at = decided_at;
    trial.completed_runs = count_trial_challenger_runs(&mut tx, user_id, &trial).await?;

    tx.commit().await.map_err(|err| {
        tracing::error!(?err, "failed to commit trial decision");
        ApiError::internal("failed to decide trial")
    })?;

    Ok(trial.into())
}

#[derive(sqlx::FromRow)]
struct ActiveTrialRow {
    id: i64,
    lineage: String,
    challenger_model: String,
    challenger_reasoning_effort: String,
    swap_percent: i32,
    allow_pr_diff: bool,
    eligible_dispatches: i32,
    lens_counters: sqlx::types::Json<HashMap<String, i32>>,
}

struct ActiveTrialState {
    id: i64,
    challenger: String,
    challenger_effort: String,
    swap_pct: i32,
    allow_pr: bool,
    counter: i32,
    role_counters: HashMap<String, i32>,
}

async fn resolve_trial_model(
    tx: &mut Transaction<'_, Postgres>,
    trials_by_lineage: &mut HashMap<String, ActiveTrialState>,
    entry: &RosterEntry,
    role: &str,
    artifact_kind: &str,
) -> Result<(String, String, bool, Option<i64>), ApiError> {
    let Some(at) = trials_by_lineage.get_mut(&entry.lineage) else {
        return Ok((
            entry.champion_model.clone(),
            entry.champion_reasoning_effort.clone(),
            false,
            None,
        ));
    };
    if artifact_kind == "pr_diff" && !at.allow_pr {
        return Ok((
            entry.champion_model.clone(),
            entry.champion_reasoning_effort.clone(),
            false,
            None,
        ));
    }

    let prev_role_count = match at.role_counters.get(role).copied() {
        Some(v) => v,
        None if role == "reviewer" => at.counter,
        None => 0,
    };
    let next_role_count = prev_role_count + 1;
    let swap = (next_role_count * at.swap_pct) / 100 > (prev_role_count * at.swap_pct) / 100;

    at.counter += 1;
    at.role_counters.insert(role.to_string(), next_role_count);

    sqlx::query(
        "UPDATE arena_trials
         SET eligible_dispatches = $2, lens_counters = $3
         WHERE id = $1",
    )
    .bind(at.id)
    .bind(at.counter)
    .bind(sqlx::types::Json(&at.role_counters))
    .execute(&mut **tx)
    .await
    .map_err(|err| {
        tracing::error!(?err, "failed to advance trial counter");
        ApiError::internal("failed to select panel")
    })?;

    if swap {
        Ok((
            at.challenger.clone(),
            at.challenger_effort.clone(),
            true,
            Some(at.id),
        ))
    } else {
        Ok((
            entry.champion_model.clone(),
            entry.champion_reasoning_effort.clone(),
            false,
            None,
        ))
    }
}

pub async fn select_panel(
    pool: &PgPool,
    user_id: Uuid,
    input: SelectPanelInput,
) -> Result<PanelSelection, ApiError> {
    let skill = input.skill.trim();
    if !is_valid_skill(skill) {
        return Err(ApiError::bad_request(format!(
            "skill must be {SKILL_ADVERSARIAL_REVIEW} or {SKILL_PLAN_TOURNAMENT}"
        )));
    }
    let artifact_kind = input
        .artifact_kind
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("other");
    if !is_valid_artifact_kind(artifact_kind) {
        return Err(ApiError::bad_request(format!(
            "invalid artifactKind {artifact_kind:?}"
        )));
    }

    let mut tx = pool.begin().await.map_err(|err| {
        tracing::error!(?err, "failed to begin select_panel transaction");
        ApiError::internal("database error")
    })?;

    lock_user_row(&mut tx, user_id).await?;

    let roster = sqlx::query_as::<_, RosterEntry>(
        "SELECT skill, lineage, champion_model, champion_reasoning_effort,
                backup_model, backup_reasoning_effort, sort_order, enabled, updated_at
         FROM arena_roster
         WHERE user_id = $1 AND skill = $2 AND enabled = TRUE
         ORDER BY sort_order ASC, lineage ASC",
    )
    .bind(user_id)
    .bind(skill)
    .fetch_all(&mut *tx)
    .await
    .map_err(|err| {
        tracing::error!(?err, "failed to query enabled roster");
        ApiError::internal("failed to select panel")
    })?;

    let explicit_host_lineage = input
        .host_lineage
        .as_deref()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty());
    let host_lineage = explicit_host_lineage
        .unwrap_or_else(|| infer_lineage(input.host_model.as_deref().unwrap_or(""), &roster));

    let mut out = PanelSelection {
        configured: !roster.is_empty(),
        run_key: format!("run_{}", &random_hex_secret()[..16]),
        skill: skill.to_string(),
        artifact_kind: artifact_kind.to_string(),
        host_lineage: host_lineage.clone(),
        rotation_turn: 0,
        legs: Vec::new(),
        critic_order: Vec::new(),
    };

    if roster.is_empty() {
        tx.commit().await.map_err(|err| {
            tracing::error!(?err, "failed to commit empty panel selection");
            ApiError::internal("failed to select panel")
        })?;
        return Ok(out);
    }

    let trial_rows = sqlx::query_as::<_, ActiveTrialRow>(
        "SELECT id, lineage, challenger_model, challenger_reasoning_effort, swap_percent,
                allow_pr_diff, eligible_dispatches, lens_counters
         FROM arena_trials
         WHERE user_id = $1 AND skill = $2 AND status = 'active'
         FOR UPDATE",
    )
    .bind(user_id)
    .bind(skill)
    .fetch_all(&mut *tx)
    .await
    .map_err(|err| {
        tracing::error!(?err, "failed to query active trials");
        ApiError::internal("failed to select panel")
    })?;

    let mut trials_by_lineage = HashMap::with_capacity(trial_rows.len());
    for row in trial_rows {
        trials_by_lineage.insert(
            row.lineage,
            ActiveTrialState {
                id: row.id,
                challenger: row.challenger_model,
                challenger_effort: row.challenger_reasoning_effort,
                swap_pct: row.swap_percent,
                allow_pr: row.allow_pr_diff,
                counter: row.eligible_dispatches,
                role_counters: row.lens_counters.0,
            },
        );
    }

    if skill == SKILL_ADVERSARIAL_REVIEW {
        let limit = if artifact_kind == "pr_diff" { 3 } else { 2 };
        let eligible: Vec<&RosterEntry> = roster
            .iter()
            .filter(|r| {
                artifact_kind == "pr_diff" || host_lineage.is_empty() || r.lineage != host_lineage
            })
            .take(limit)
            .collect();

        for (i, entry) in eligible.into_iter().enumerate() {
            let (model, reasoning_effort, is_challenger, trial_id) = resolve_trial_model(
                &mut tx,
                &mut trials_by_lineage,
                entry,
                "reviewer",
                artifact_kind,
            )
            .await?;
            out.legs.push(PanelLeg {
                slot_index: i as i32,
                role: "reviewer".to_string(),
                lineage: entry.lineage.clone(),
                model,
                reasoning_effort,
                backup_model: entry.backup_model.clone(),
                backup_reasoning_effort: entry.backup_reasoning_effort.clone(),
                is_challenger,
                trial_id,
            });
        }
    } else {
        let turn: i64 = sqlx::query_scalar(
            "INSERT INTO arena_rotation_state (user_id, skill, turn_count)
             VALUES ($1, $2, 1)
             ON CONFLICT (user_id, skill) DO UPDATE
             SET turn_count = arena_rotation_state.turn_count + 1,
                 updated_at = NOW()
             RETURNING turn_count - 1",
        )
        .bind(user_id)
        .bind(skill)
        .fetch_one(&mut *tx)
        .await
        .map_err(|err| {
            tracing::error!(?err, "failed to advance rotation state");
            ApiError::internal("failed to select panel")
        })?;
        out.rotation_turn = turn;

        let n = roster.len();
        for (i, lens) in TOURNAMENT_LENSES.iter().enumerate() {
            let entry = &roster[((turn as usize) + i) % n];
            let (model, reasoning_effort, is_challenger, trial_id) =
                resolve_trial_model(&mut tx, &mut trials_by_lineage, entry, lens, artifact_kind)
                    .await?;
            if !out.critic_order.contains(&model) {
                out.critic_order.push(model.clone());
            }
            out.legs.push(PanelLeg {
                slot_index: i as i32,
                role: (*lens).to_string(),
                lineage: entry.lineage.clone(),
                model,
                reasoning_effort,
                backup_model: entry.backup_model.clone(),
                backup_reasoning_effort: entry.backup_reasoning_effort.clone(),
                is_challenger,
                trial_id,
            });
        }
    }

    tx.commit().await.map_err(|err| {
        tracing::error!(?err, "failed to commit panel selection");
        ApiError::internal("failed to select panel")
    })?;

    Ok(out)
}

pub fn validate_and_normalize_record_run_input(input: &mut RecordRunInput) -> Result<(), ApiError> {
    input.run_key = input.run_key.trim().to_string();
    input.skill = input.skill.trim().to_string();
    if input.run_key.is_empty() {
        return Err(ApiError::bad_request("runKey is required"));
    }
    if !is_valid_skill(&input.skill) {
        return Err(ApiError::bad_request(format!(
            "skill must be {SKILL_ADVERSARIAL_REVIEW} or {SKILL_PLAN_TOURNAMENT}"
        )));
    }

    input.artifact_kind = input.artifact_kind.trim().to_string();
    if input.artifact_kind.is_empty() {
        input.artifact_kind = "other".to_string();
    }
    if !is_valid_artifact_kind(&input.artifact_kind) {
        return Err(ApiError::bad_request(format!(
            "invalid artifactKind {:?}",
            input.artifact_kind
        )));
    }

    input.phase = input.phase.trim().to_string();
    if input.phase.is_empty() {
        input.phase = "independent".to_string();
    }
    if !VALID_PHASES.contains(&input.phase.as_str()) {
        return Err(ApiError::bad_request(format!(
            "invalid phase {:?}",
            input.phase
        )));
    }

    input.approach_verdict = input.approach_verdict.trim().to_string();
    if !VALID_APPROACH_VERDICTS.contains(&input.approach_verdict.as_str()) {
        return Err(ApiError::bad_request(format!(
            "invalid approachVerdict {:?}",
            input.approach_verdict
        )));
    }

    input.winning_role = input.winning_role.trim().to_string();
    if !VALID_WINNING_ROLES.contains(&input.winning_role.as_str()) {
        return Err(ApiError::bad_request(format!(
            "invalid winningRole {:?}",
            input.winning_role
        )));
    }
    if input.skill == SKILL_ADVERSARIAL_REVIEW && !input.winning_role.is_empty() {
        return Err(ApiError::bad_request(
            "winningRole is only valid for plan-tournament runs",
        ));
    }
    if input.skill == SKILL_PLAN_TOURNAMENT && !input.approach_verdict.is_empty() {
        return Err(ApiError::bad_request(
            "approachVerdict is only valid for adversarial-review runs",
        ));
    }

    input.artifact_ref = input.artifact_ref.trim().to_string();
    input.repo = input.repo.trim().to_string();
    input.host_model = input.host_model.trim().to_string();
    input.host_reasoning_effort =
        normalize_reasoning_effort(&input.host_reasoning_effort).map_err(ApiError::bad_request)?;
    input.host_lineage = input.host_lineage.trim().to_ascii_lowercase();
    input.harness = input.harness.trim().to_string();
    input.machine = input.machine.trim().to_string();

    if input.legs.is_empty() {
        return Err(ApiError::bad_request(
            "legs must contain at least one subagent leg",
        ));
    }

    let mut seen_slots = HashSet::with_capacity(input.legs.len());
    for leg in &mut input.legs {
        leg.role = leg.role.trim().to_string();
        leg.model = leg.model.trim().to_string();
        leg.reasoning_effort =
            normalize_reasoning_effort(&leg.reasoning_effort).map_err(ApiError::bad_request)?;
        leg.lineage = leg.lineage.trim().to_ascii_lowercase();
        leg.status = leg.status.trim().to_string();
        if leg.status.is_empty() {
            leg.status = "ok".to_string();
        }
        if leg.slot_index < 0 {
            return Err(ApiError::bad_request(format!(
                "leg slotIndex must be >= 0 (got {})",
                leg.slot_index
            )));
        }
        if !seen_slots.insert(leg.slot_index) {
            return Err(ApiError::bad_request(format!(
                "duplicate leg slotIndex {}",
                leg.slot_index
            )));
        }
        if leg.role.is_empty() || leg.model.is_empty() {
            return Err(ApiError::bad_request(
                "each leg must specify role and model",
            ));
        }
        if !VALID_LEG_ROLES.contains(&leg.role.as_str()) {
            return Err(ApiError::bad_request(format!(
                "invalid leg role {:?}",
                leg.role
            )));
        }
        if !VALID_LEG_STATUSES.contains(&leg.status.as_str()) {
            return Err(ApiError::bad_request(format!(
                "invalid leg status {:?}",
                leg.status
            )));
        }
        if input.skill == SKILL_ADVERSARIAL_REVIEW
            && !matches!(leg.role.as_str(), "reviewer" | "ponytail")
        {
            return Err(ApiError::bad_request(format!(
                "leg role {:?} is not valid for adversarial-review",
                leg.role
            )));
        }
        if input.skill == SKILL_PLAN_TOURNAMENT
            && !matches!(
                leg.role.as_str(),
                "minimal_diff" | "clean_architecture" | "edge_cases" | "critic" | "explore_scout"
            )
        {
            return Err(ApiError::bad_request(format!(
                "leg role {:?} is not valid for plan-tournament",
                leg.role
            )));
        }
        if !(0..=86_400_000).contains(&leg.duration_ms)
            || !(0..=1_000).contains(&leg.cherry_picks_count)
            || !(0..=1_000).contains(&leg.critic_adopted_count)
            || !(0..=1_000).contains(&leg.critic_bloat_dropped_count)
        {
            return Err(ApiError::bad_request(
                "leg counts must be 0..=1000 and durationMs must be 0..=86400000",
            ));
        }
    }

    if !input.candidates.is_empty() && input.skill != SKILL_ADVERSARIAL_REVIEW {
        return Err(ApiError::bad_request(
            "candidates are only valid for adversarial-review runs",
        ));
    }

    let mut reviewer_models: HashMap<String, String> = HashMap::new();
    let mut has_ponytail_leg = false;
    for leg in &input.legs {
        match leg.role.as_str() {
            "reviewer" if matches!(leg.status.as_str(), "ok" | "retried") => {
                let lower = leg.model.to_ascii_lowercase();
                if let Some(prev_effort) = reviewer_models.get(&lower)
                    && prev_effort != &leg.reasoning_effort
                {
                    return Err(ApiError::bad_request(format!(
                        "ambiguous reviewer legs for model {:?}: conflicting reasoningEffort {:?} vs {:?}",
                        leg.model, prev_effort, leg.reasoning_effort
                    )));
                }
                reviewer_models.insert(lower, leg.reasoning_effort.clone());
            }
            "ponytail" => {
                has_ponytail_leg = true;
            }
            _ => {}
        }
    }

    for (idx, c) in input.candidates.iter_mut().enumerate() {
        c.candidate_key = c.candidate_key.trim().to_string();
        c.summary = c.summary.trim().to_string();
        c.severity = c.severity.trim().to_string();
        if c.severity.is_empty() {
            c.severity = "important".to_string();
        }
        c.disposition = c.disposition.trim().to_string();
        if !VALID_CANDIDATE_SEVERITIES.contains(&c.severity.as_str()) {
            return Err(ApiError::bad_request(format!(
                "invalid candidate severity {:?}",
                c.severity
            )));
        }
        if !VALID_CANDIDATE_DISPOSITIONS.contains(&c.disposition.as_str()) {
            return Err(ApiError::bad_request(format!(
                "invalid candidate disposition {:?}",
                c.disposition
            )));
        }

        let mut cleaned = Vec::with_capacity(c.proposing_models.len());
        let mut seen_proposers = HashSet::with_capacity(c.proposing_models.len());
        for m in &c.proposing_models {
            let trimmed = m.trim();
            if trimmed.is_empty() {
                continue;
            }
            let lower = trimmed.to_ascii_lowercase();
            if lower == "ponytail" {
                if !has_ponytail_leg {
                    return Err(ApiError::bad_request(format!(
                        "candidate {idx} proposer {trimmed:?} requires a ponytail leg in legs"
                    )));
                }
            } else if !reviewer_models.contains_key(&lower) {
                return Err(ApiError::bad_request(format!(
                    "candidate {idx} proposer {trimmed:?} does not match any reviewer leg in legs"
                )));
            }
            if seen_proposers.insert(lower) {
                cleaned.push(trimmed.to_string());
            }
        }
        if cleaned.is_empty() {
            return Err(ApiError::bad_request(format!(
                "candidate {idx} must have at least one proposingModel"
            )));
        }
        c.proposing_models = cleaned;
    }

    let derived_keys = derive_candidate_keys(&input.candidates).map_err(ApiError::bad_request)?;
    for (c, key) in input.candidates.iter_mut().zip(derived_keys) {
        c.candidate_key = key;
    }

    Ok(())
}

#[derive(sqlx::FromRow)]
struct ExistingRunMetaRow {
    id: i64,
    skill: String,
    occurred_at: DateTime<Utc>,
    prev_sent_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct PreservedLegTrialRow {
    slot_index: i32,
    role: String,
    lineage: String,
    model: String,
    reasoning_effort: String,
    is_challenger: bool,
    trial_id: i64,
}

#[derive(sqlx::FromRow)]
struct TrialLookupRow {
    id: i64,
    skill: String,
    lineage: String,
    champion_model: String,
    champion_reasoning_effort: String,
    challenger_model: String,
    challenger_reasoning_effort: String,
    allow_pr_diff: bool,
    created_at: DateTime<Utc>,
    decided_at: Option<DateTime<Utc>>,
}

pub async fn record_run(
    pool: &PgPool,
    user_id: Uuid,
    mut input: RecordRunInput,
) -> Result<RecordedRunOutput, ApiError> {
    validate_and_normalize_record_run_input(&mut input)?;

    let mut tx = pool.begin().await.map_err(|err| {
        tracing::error!(?err, "failed to begin record_run transaction");
        ApiError::internal("database error")
    })?;

    lock_user_row(&mut tx, user_id).await?;

    let roster = sqlx::query_as::<_, RosterEntry>(
        "SELECT skill, lineage, champion_model, champion_reasoning_effort,
                backup_model, backup_reasoning_effort, sort_order, enabled, updated_at
         FROM arena_roster
         WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(|err| {
        tracing::error!(?err, "failed to query roster in record_run");
        ApiError::internal("database error")
    })?;

    if input.host_lineage.is_empty() {
        input.host_lineage = infer_lineage(&input.host_model, &roster);
    }
    for leg in &mut input.legs {
        if leg.lineage.is_empty() {
            leg.lineage = infer_lineage(&leg.model, &roster);
        }
    }

    let existing = sqlx::query_as::<_, ExistingRunMetaRow>(
        "SELECT id, skill, occurred_at, COALESCE(client_sent_at, recorded_at) AS prev_sent_at
         FROM arena_runs
         WHERE user_id = $1 AND run_key = $2
         FOR UPDATE",
    )
    .bind(user_id)
    .bind(&input.run_key)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|err| {
        tracing::error!(?err, "failed to query existing run");
        ApiError::internal("database error")
    })?;

    let mut affected_skills = vec![input.skill.clone()];
    if let Some(prev) = &existing {
        if prev.skill != input.skill {
            affected_skills.push(prev.skill.clone());
        }
        if let Some(client_sent_at) = input.client_sent_at
            && client_sent_at < prev.prev_sent_at
        {
            tx.commit().await.map_err(|err| {
                tracing::error!(?err, "failed to commit out-of-order replay no-op");
                ApiError::internal("database error")
            })?;
            return Ok(RecordedRunOutput {
                run_id: prev.id,
                run_key: input.run_key,
                completed_trials: Vec::new(),
                spooled: false,
            });
        }
    }

    let occurred_at = input
        .occurred_at
        .or_else(|| existing.as_ref().map(|prev| prev.occurred_at))
        .unwrap_or_else(Utc::now);

    let run_id: i64 = sqlx::query_scalar(
        "INSERT INTO arena_runs (
            user_id, run_key, skill, artifact_kind, artifact_ref, repo, phase,
            host_model, host_reasoning_effort, host_lineage, harness, machine,
            approach_verdict, winning_role, occurred_at, client_sent_at, recorded_at
         )
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, COALESCE($16, NOW()), NOW())
         ON CONFLICT (user_id, run_key) DO UPDATE SET
            skill = EXCLUDED.skill,
            artifact_kind = EXCLUDED.artifact_kind,
            artifact_ref = EXCLUDED.artifact_ref,
            repo = EXCLUDED.repo,
            phase = EXCLUDED.phase,
            host_model = EXCLUDED.host_model,
            host_reasoning_effort = EXCLUDED.host_reasoning_effort,
            host_lineage = EXCLUDED.host_lineage,
            harness = EXCLUDED.harness,
            machine = EXCLUDED.machine,
            approach_verdict = EXCLUDED.approach_verdict,
            winning_role = EXCLUDED.winning_role,
            occurred_at = EXCLUDED.occurred_at,
            client_sent_at = COALESCE($16, NOW()),
            recorded_at = NOW()
         RETURNING id",
    )
    .bind(user_id)
    .bind(&input.run_key)
    .bind(&input.skill)
    .bind(&input.artifact_kind)
    .bind(&input.artifact_ref)
    .bind(&input.repo)
    .bind(&input.phase)
    .bind(&input.host_model)
    .bind(&input.host_reasoning_effort)
    .bind(&input.host_lineage)
    .bind(&input.harness)
    .bind(&input.machine)
    .bind(&input.approach_verdict)
    .bind(&input.winning_role)
    .bind(occurred_at)
    .bind(input.client_sent_at)
    .fetch_one(&mut *tx)
    .await
    .map_err(|err| {
        tracing::error!(?err, "failed to upsert arena_runs");
        ApiError::internal("failed to record run")
    })?;

    let open_trials = sqlx::query_as::<_, TrialRow>(
        "SELECT
            id, skill, lineage, champion_model, champion_reasoning_effort,
            challenger_model, challenger_reasoning_effort,
            target_runs, 0::INT AS completed_runs, swap_percent, allow_pr_diff,
            status, created_at, decided_at
         FROM arena_trials
         WHERE user_id = $1 AND skill = ANY($2) AND status IN ('active', 'completed')
         ORDER BY id ASC
         FOR UPDATE",
    )
    .bind(user_id)
    .bind(&affected_skills)
    .fetch_all(&mut *tx)
    .await
    .map_err(|err| {
        tracing::error!(?err, "failed to lock open trials");
        ApiError::internal("failed to record run")
    })?;

    let mut open_trial_by_lineage = HashMap::new();
    for t in &open_trials {
        if t.skill == input.skill {
            open_trial_by_lineage.insert(t.lineage.clone(), t.clone());
        }
    }

    let preserved_rows = sqlx::query_as::<_, PreservedLegTrialRow>(
        "SELECT slot_index, role, lineage, model, reasoning_effort, is_challenger, trial_id
         FROM arena_run_legs
         WHERE run_id = $1 AND trial_id IS NOT NULL",
    )
    .bind(run_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(|err| {
        tracing::error!(?err, "failed to read preserved leg trials");
        ApiError::internal("failed to record run")
    })?;

    let mut preserved_by_slot = HashMap::with_capacity(preserved_rows.len());
    for row in preserved_rows {
        preserved_by_slot.insert(row.slot_index, row);
    }

    sqlx::query("DELETE FROM arena_run_legs WHERE run_id = $1")
        .bind(run_id)
        .execute(&mut *tx)
        .await
        .map_err(|err| {
            tracing::error!(?err, "failed to clear old run legs");
            ApiError::internal("failed to record run")
        })?;

    for leg in &mut input.legs {
        if let Some(tid) = leg.trial_id {
            if tid <= 0 {
                return Err(ApiError::bad_request("leg trialId must be > 0"));
            }
            let t = sqlx::query_as::<_, TrialLookupRow>(
                "SELECT id, skill, lineage, champion_model, champion_reasoning_effort,
                        challenger_model, challenger_reasoning_effort,
                        allow_pr_diff, created_at, decided_at
                 FROM arena_trials
                 WHERE id = $1 AND user_id = $2",
            )
            .bind(tid)
            .bind(user_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|err| {
                tracing::error!(?err, "failed to look up leg trial");
                ApiError::internal("database error")
            })?
            .ok_or_else(|| ApiError::bad_request(format!("leg trialId {tid} not found")))?;

            if t.skill != input.skill {
                return Err(ApiError::bad_request(format!(
                    "leg trialId {} belongs to skill {:?}, not {:?}",
                    t.id, t.skill, input.skill
                )));
            }
            if !is_trial_competing_role(&input.skill, &leg.role) {
                return Err(ApiError::bad_request(format!(
                    "leg role {:?} cannot be attached to trial {}",
                    leg.role, t.id
                )));
            }
            if t.lineage != leg.lineage {
                return Err(ApiError::bad_request(format!(
                    "leg trialId {} belongs to lineage {:?}, not {:?}",
                    t.id, t.lineage, leg.lineage
                )));
            }
            if !t.allow_pr_diff && input.artifact_kind == "pr_diff" {
                return Err(ApiError::bad_request(format!(
                    "trial {} does not allow pr_diff runs",
                    t.id
                )));
            }
            if occurred_at < t.created_at {
                return Err(ApiError::bad_request(format!(
                    "run occurredAt precedes trial {} creation",
                    t.id
                )));
            }
            if let Some(decided_at) = t.decided_at
                && occurred_at > decided_at
            {
                return Err(ApiError::bad_request(format!(
                    "run occurredAt is after trial {} decision",
                    t.id
                )));
            }
            let leg_key = model_effort_key(&leg.model, &leg.reasoning_effort);
            if leg_key == model_effort_key(&t.challenger_model, &t.challenger_reasoning_effort) {
                leg.is_challenger = true;
            } else if leg_key == model_effort_key(&t.champion_model, &t.champion_reasoning_effort) {
                leg.is_challenger = false;
            } else {
                return Err(ApiError::bad_request(format!(
                    "leg model {:?} (effort {:?}) does not match champion {:?} ({:?}) or challenger {:?} ({:?}) on trial {}",
                    leg.model,
                    leg.reasoning_effort,
                    t.champion_model,
                    t.champion_reasoning_effort,
                    t.challenger_model,
                    t.challenger_reasoning_effort,
                    t.id
                )));
            }
        } else {
            leg.is_challenger = false;
            if is_trial_competing_role(&input.skill, &leg.role) {
                if let Some(prev) = preserved_by_slot.get(&leg.slot_index)
                    && prev.role == leg.role
                    && prev.lineage == leg.lineage
                    && model_effort_key(&prev.model, &prev.reasoning_effort)
                        == model_effort_key(&leg.model, &leg.reasoning_effort)
                {
                    leg.trial_id = Some(prev.trial_id);
                    leg.is_challenger = prev.is_challenger;
                }
                if leg.trial_id.is_none()
                    && let Some(t) = open_trial_by_lineage.get(&leg.lineage)
                    && (t.allow_pr_diff || input.artifact_kind != "pr_diff")
                    && occurred_at >= t.created_at
                {
                    let leg_key = model_effort_key(&leg.model, &leg.reasoning_effort);
                    if leg_key
                        == model_effort_key(&t.challenger_model, &t.challenger_reasoning_effort)
                    {
                        leg.trial_id = Some(t.id);
                        leg.is_challenger = true;
                    } else if leg_key
                        == model_effort_key(&t.champion_model, &t.champion_reasoning_effort)
                    {
                        leg.trial_id = Some(t.id);
                        leg.is_challenger = false;
                    }
                }
            }
        }

        sqlx::query(
            "INSERT INTO arena_run_legs (
                run_id, slot_index, role, lineage, model, reasoning_effort, is_challenger, trial_id,
                status, duration_ms, won_core_plan, cherry_picks_count,
                critic_adopted_count, critic_bloat_dropped_count
             )
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
        )
        .bind(run_id)
        .bind(leg.slot_index)
        .bind(&leg.role)
        .bind(&leg.lineage)
        .bind(&leg.model)
        .bind(&leg.reasoning_effort)
        .bind(leg.is_challenger)
        .bind(leg.trial_id)
        .bind(&leg.status)
        .bind(leg.duration_ms)
        .bind(leg.won_core_plan)
        .bind(leg.cherry_picks_count)
        .bind(leg.critic_adopted_count)
        .bind(leg.critic_bloat_dropped_count)
        .execute(&mut *tx)
        .await
        .map_err(|err| {
            tracing::error!(?err, "failed to insert arena_run_leg");
            ApiError::internal("failed to record run")
        })?;
    }

    let mut keep_keys = Vec::with_capacity(input.candidates.len());
    for (idx, c) in input.candidates.iter().enumerate() {
        keep_keys.push(c.candidate_key.clone());
        sqlx::query(
            "INSERT INTO arena_candidates (
                run_id, candidate_index, candidate_key, summary, severity,
                disposition, caused_approach_pivot, proposing_models
             )
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (run_id, candidate_key) DO UPDATE SET
                candidate_index = EXCLUDED.candidate_index,
                summary = EXCLUDED.summary,
                severity = EXCLUDED.severity,
                disposition = EXCLUDED.disposition,
                caused_approach_pivot = EXCLUDED.caused_approach_pivot,
                proposing_models = EXCLUDED.proposing_models",
        )
        .bind(run_id)
        .bind(idx as i32)
        .bind(&c.candidate_key)
        .bind(&c.summary)
        .bind(&c.severity)
        .bind(&c.disposition)
        .bind(c.caused_approach_pivot)
        .bind(&c.proposing_models)
        .execute(&mut *tx)
        .await
        .map_err(|err| {
            tracing::error!(?err, "failed to upsert arena_candidate");
            ApiError::internal("failed to record run")
        })?;
    }

    if keep_keys.is_empty() {
        sqlx::query("DELETE FROM arena_candidates WHERE run_id = $1")
            .bind(run_id)
            .execute(&mut *tx)
            .await
            .map_err(|err| {
                tracing::error!(?err, "failed to clear arena_candidates");
                ApiError::internal("failed to record run")
            })?;
    } else {
        sqlx::query(
            "DELETE FROM arena_candidates WHERE run_id = $1 AND NOT (candidate_key = ANY($2))",
        )
        .bind(run_id)
        .bind(&keep_keys)
        .execute(&mut *tx)
        .await
        .map_err(|err| {
            tracing::error!(?err, "failed to prune removed arena_candidates");
            ApiError::internal("failed to record run")
        })?;
    }

    let mut completed_trials = Vec::new();
    for mut t in open_trials {
        let count = count_trial_challenger_runs(&mut tx, user_id, &t).await?;
        if t.status == "completed" && count < t.target_runs {
            sqlx::query("UPDATE arena_trials SET status = 'active' WHERE id = $1")
                .bind(t.id)
                .execute(&mut *tx)
                .await
                .map_err(|err| {
                    tracing::error!(?err, "failed to reopen trial");
                    ApiError::internal("failed to record run")
                })?;
        } else if t.status == "active" && count >= t.target_runs {
            sqlx::query("UPDATE arena_trials SET status = 'completed' WHERE id = $1")
                .bind(t.id)
                .execute(&mut *tx)
                .await
                .map_err(|err| {
                    tracing::error!(?err, "failed to complete trial");
                    ApiError::internal("failed to record run")
                })?;
            t.status = "completed".to_string();
            t.completed_runs = count;
            completed_trials.push(Trial::from(t));
        }
    }

    tx.commit().await.map_err(|err| {
        tracing::error!(?err, "failed to commit record_run");
        ApiError::internal("failed to record run")
    })?;

    Ok(RecordedRunOutput {
        run_id,
        run_key: input.run_key,
        completed_trials,
        spooled: false,
    })
}

pub async fn override_candidate(
    pool: &PgPool,
    user_id: Uuid,
    candidate_id: i64,
    input: OverrideCandidateInput,
) -> Result<OverrideCandidateResponse, ApiError> {
    let val = input.human_override.trim();
    if !matches!(val, "confirmed" | "overturned" | "clear" | "") {
        return Err(ApiError::bad_request(
            "humanOverride must be confirmed, overturned, or clear",
        ));
    }
    let stored = if val == "clear" { "" } else { val };
    let note = input.note.trim();

    let mut tx = pool.begin().await.map_err(|err| {
        tracing::error!(?err, "failed to begin override_candidate transaction");
        ApiError::internal("database error")
    })?;

    lock_user_row(&mut tx, user_id).await?;

    let updated = sqlx::query(
        "UPDATE arena_candidates c
         SET human_override = $3,
             override_note = $4
         FROM arena_runs r
         WHERE c.id = $1 AND c.run_id = r.id AND r.user_id = $2",
    )
    .bind(candidate_id)
    .bind(user_id)
    .bind(stored)
    .bind(note)
    .execute(&mut *tx)
    .await
    .map_err(|err| {
        tracing::error!(?err, "failed to override candidate");
        ApiError::internal("failed to override candidate")
    })?;

    if updated.rows_affected() == 0 {
        return Err(ApiError::not_found(format!(
            "candidate {candidate_id} not found"
        )));
    }

    tx.commit().await.map_err(|err| {
        tracing::error!(?err, "failed to commit override_candidate");
        ApiError::internal("failed to override candidate")
    })?;

    Ok(OverrideCandidateResponse {
        id: candidate_id,
        human_override: stored.to_string(),
    })
}

pub fn validate_and_normalize_coding_run_input(
    input: &mut CodingRunInput,
) -> Result<String, ApiError> {
    input.run_key = input.run_key.trim().to_string();
    if input.run_key.is_empty() || input.run_key.len() > 256 {
        return Err(ApiError::bad_request(
            "runKey must be between 1 and 256 characters",
        ));
    }

    input.model = input.model.trim().to_string();
    if input.model.is_empty() || input.model.len() > 128 {
        return Err(ApiError::bad_request(
            "model must be between 1 and 128 characters",
        ));
    }

    input.reasoning_effort =
        normalize_reasoning_effort(&input.reasoning_effort).map_err(ApiError::bad_request)?;

    input.lineage = input.lineage.trim().to_ascii_lowercase();
    if input.lineage.len() > 128 {
        return Err(ApiError::bad_request(
            "lineage must be at most 128 characters",
        ));
    }

    input.harness = input.harness.trim().to_string();
    if input.harness.len() > 128 {
        return Err(ApiError::bad_request(
            "harness must be at most 128 characters",
        ));
    }

    input.repo = input.repo.trim().to_string();
    if input.repo.len() > 256 {
        return Err(ApiError::bad_request("repo must be at most 256 characters"));
    }

    if input.pr_number < 0 {
        return Err(ApiError::bad_request("prNumber cannot be negative"));
    }

    input.issue_ref = input.issue_ref.trim().to_string();
    if input.issue_ref.len() > 256 {
        return Err(ApiError::bad_request(
            "issueRef must be at most 256 characters",
        ));
    }

    input.task_kind = input.task_kind.trim().to_ascii_lowercase();
    if input.task_kind.is_empty() {
        input.task_kind = "feature".to_string();
    } else if !VALID_TASK_KINDS.contains(&input.task_kind.as_str()) {
        return Err(ApiError::bad_request(format!(
            "invalid taskKind {:?}: must be one of {:?}",
            input.task_kind, VALID_TASK_KINDS
        )));
    }

    if !(0..=1_000_000).contains(&input.loc_added)
        || !(0..=1_000_000).contains(&input.loc_deleted)
        || !(0..=100_000).contains(&input.files_changed)
        || !(-1_000_000..=1_000_000).contains(&input.tests_delta)
    {
        return Err(ApiError::bad_request(
            "locAdded, locDeleted, filesChanged, and testsDelta are out of bounds",
        ));
    }

    if !(0..=10_000).contains(&input.prompt_turn_count)
        || !(0..=10_000).contains(&input.red_ci_pushes)
        || !(0..=10_000).contains(&input.ccr_rounds)
        || !(0..=10_000).contains(&input.ccr_actionable_count)
        || !(0..=10_000).contains(&input.ccr_noise_count)
        || !(0..=10_000).contains(&input.human_threads_count)
    {
        return Err(ApiError::bad_request(
            "turn, CI, CCR, and review thread counts must be between 0 and 10000",
        ));
    }

    const MAX_CODING_DURATION_SEC: i32 = 604_800; // 7 days in seconds
    if !(0..=MAX_CODING_DURATION_SEC).contains(&input.time_to_green_sec)
        || !(0..=MAX_CODING_DURATION_SEC).contains(&input.time_to_merge_sec)
    {
        return Err(ApiError::bad_request(format!(
            "timeToGreenSec and timeToMergeSec must be between 0 and {MAX_CODING_DURATION_SEC}"
        )));
    }

    input.digest_summary = input.digest_summary.trim().to_string();
    if input.digest_summary.len() > 4096 {
        return Err(ApiError::bad_request(
            "digestSummary must be at most 4096 characters",
        ));
    }

    Ok(classify_size_bucket(input.loc_added, input.loc_deleted).to_string())
}

/// Records or updates a single-model coding run idempotently on `(user_id, run_key)`.
///
/// Stale replays (`client_sent_at < existing.client_sent_at`) return the existing record without
/// overwriting newer metrics, and human overrides (`human_override`, `override_note`) are preserved
/// across re-submissions.
pub async fn record_coding_run(
    pool: &PgPool,
    user_id: Uuid,
    mut input: CodingRunInput,
) -> Result<RecordedCodingRunOutput, ApiError> {
    let size_bucket = validate_and_normalize_coding_run_input(&mut input)?;

    let mut tx = pool.begin().await.map_err(|err| {
        tracing::error!(?err, "failed to begin record_coding_run transaction");
        ApiError::internal("database error")
    })?;

    lock_user_row(&mut tx, user_id).await?;

    if input.lineage.is_empty() {
        let roster: Vec<RosterEntry> = sqlx::query_as(
            "SELECT skill, lineage, champion_model, champion_reasoning_effort,
                    backup_model, backup_reasoning_effort, sort_order, enabled, updated_at
             FROM arena_roster
             WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(|err| {
            tracing::error!(?err, "failed to query roster for coding run lineage");
            ApiError::internal("failed to record coding run")
        })?;
        input.lineage = infer_lineage(&input.model, &roster);
    }

    let upserted: Option<(i64, String)> = sqlx::query_as(
        "INSERT INTO arena_coding_runs (
            user_id, run_key, model, reasoning_effort, lineage, harness, repo, pr_number, issue_ref,
            task_kind, loc_added, loc_deleted, size_bucket, files_changed, tests_delta,
            prompt_turn_count, red_ci_pushes, ccr_rounds, ccr_actionable_count, ccr_noise_count,
            human_threads_count, escaped_regression, merged_without_followup_24h, first_pass_clean,
            abandoned, time_to_green_sec, time_to_merge_sec, digest_summary,
            occurred_at, client_sent_at, recorded_at
         ) VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9,
            $10, $11, $12, $13, $14, $15,
            $16, $17, $18, $19, $20,
            $21, $22, $23, $24,
            $25, $26, $27, $28,
            COALESCE($29, NOW()), COALESCE($30, NOW()), NOW()
         )
         ON CONFLICT (user_id, run_key) DO UPDATE SET
            model = EXCLUDED.model,
            reasoning_effort = EXCLUDED.reasoning_effort,
            lineage = EXCLUDED.lineage,
            harness = EXCLUDED.harness,
            repo = EXCLUDED.repo,
            pr_number = EXCLUDED.pr_number,
            issue_ref = EXCLUDED.issue_ref,
            task_kind = EXCLUDED.task_kind,
            loc_added = EXCLUDED.loc_added,
            loc_deleted = EXCLUDED.loc_deleted,
            size_bucket = EXCLUDED.size_bucket,
            files_changed = EXCLUDED.files_changed,
            tests_delta = EXCLUDED.tests_delta,
            prompt_turn_count = EXCLUDED.prompt_turn_count,
            red_ci_pushes = EXCLUDED.red_ci_pushes,
            ccr_rounds = EXCLUDED.ccr_rounds,
            ccr_actionable_count = EXCLUDED.ccr_actionable_count,
            ccr_noise_count = EXCLUDED.ccr_noise_count,
            human_threads_count = EXCLUDED.human_threads_count,
            escaped_regression = EXCLUDED.escaped_regression,
            merged_without_followup_24h = EXCLUDED.merged_without_followup_24h,
            first_pass_clean = EXCLUDED.first_pass_clean,
            abandoned = EXCLUDED.abandoned,
            time_to_green_sec = EXCLUDED.time_to_green_sec,
            time_to_merge_sec = EXCLUDED.time_to_merge_sec,
            digest_summary = EXCLUDED.digest_summary,
            occurred_at = COALESCE($29, arena_coding_runs.occurred_at),
            client_sent_at = COALESCE($30, NOW()),
            recorded_at = NOW()
         WHERE arena_coding_runs.client_sent_at IS NULL
            OR $30 IS NULL
            OR $30 >= arena_coding_runs.client_sent_at
         RETURNING id, size_bucket",
    )
    .bind(user_id)
    .bind(&input.run_key)
    .bind(&input.model)
    .bind(&input.reasoning_effort)
    .bind(&input.lineage)
    .bind(&input.harness)
    .bind(&input.repo)
    .bind(input.pr_number)
    .bind(&input.issue_ref)
    .bind(&input.task_kind)
    .bind(input.loc_added)
    .bind(input.loc_deleted)
    .bind(&size_bucket)
    .bind(input.files_changed)
    .bind(input.tests_delta)
    .bind(input.prompt_turn_count)
    .bind(input.red_ci_pushes)
    .bind(input.ccr_rounds)
    .bind(input.ccr_actionable_count)
    .bind(input.ccr_noise_count)
    .bind(input.human_threads_count)
    .bind(input.escaped_regression)
    .bind(input.merged_without_followup_24h)
    .bind(input.first_pass_clean)
    .bind(input.abandoned)
    .bind(input.time_to_green_sec)
    .bind(input.time_to_merge_sec)
    .bind(&input.digest_summary)
    .bind(input.occurred_at)
    .bind(input.client_sent_at)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|err| {
        tracing::error!(?err, "failed to upsert coding run");
        ApiError::internal("failed to record coding run")
    })?;

    let (run_id, stored_bucket) =
        match upserted {
            Some(pair) => pair,
            None => {
                let existing: (i64, String) = sqlx::query_as(
                "SELECT id, size_bucket FROM arena_coding_runs WHERE user_id = $1 AND run_key = $2",
            )
            .bind(user_id)
            .bind(&input.run_key)
            .fetch_one(&mut *tx)
            .await
            .map_err(|err| {
                tracing::error!(?err, "failed to fetch existing coding run after stale replay");
                ApiError::internal("failed to record coding run")
            })?;
                tx.commit().await.map_err(|err| {
                    tracing::error!(?err, "failed to commit stale coding run replay");
                    ApiError::internal("failed to record coding run")
                })?;
                return Ok(RecordedCodingRunOutput {
                    run_id: existing.0,
                    run_key: input.run_key,
                    size_bucket: existing.1,
                    spooled: false,
                });
            }
        };

    tx.commit().await.map_err(|err| {
        tracing::error!(?err, "failed to commit record_coding_run");
        ApiError::internal("failed to record coding run")
    })?;

    Ok(RecordedCodingRunOutput {
        run_id,
        run_key: input.run_key,
        size_bucket: stored_bucket,
        spooled: false,
    })
}

pub async fn override_coding_run(
    pool: &PgPool,
    user_id: Uuid,
    coding_run_id: i64,
    input: OverrideCodingRunInput,
) -> Result<OverrideCodingRunResponse, ApiError> {
    if coding_run_id <= 0 {
        return Err(ApiError::bad_request("coding run id must be positive"));
    }

    let raw_override = input.human_override.trim().to_ascii_lowercase();
    let stored = match raw_override.as_str() {
        "confirmed" => "confirmed",
        "overturned" => "overturned",
        "clear" | "" => "",
        other => {
            return Err(ApiError::bad_request(format!(
                "invalid humanOverride {other:?}: must be confirmed, overturned, or clear"
            )));
        }
    };

    let note = if stored.is_empty() && input.note.trim().is_empty() {
        ""
    } else {
        input.note.trim()
    };
    if note.len() > 2000 {
        return Err(ApiError::bad_request(
            "note must be at most 2000 characters",
        ));
    }

    let mut tx = pool.begin().await.map_err(|err| {
        tracing::error!(?err, "failed to begin override_coding_run transaction");
        ApiError::internal("database error")
    })?;

    lock_user_row(&mut tx, user_id).await?;

    let updated = sqlx::query(
        "UPDATE arena_coding_runs
         SET human_override = $3,
             override_note = $4
         WHERE id = $1 AND user_id = $2",
    )
    .bind(coding_run_id)
    .bind(user_id)
    .bind(stored)
    .bind(note)
    .execute(&mut *tx)
    .await
    .map_err(|err| {
        tracing::error!(?err, "failed to override coding run");
        ApiError::internal("failed to override coding run")
    })?;

    if updated.rows_affected() == 0 {
        return Err(ApiError::not_found(format!(
            "coding run {coding_run_id} not found"
        )));
    }

    tx.commit().await.map_err(|err| {
        tracing::error!(?err, "failed to commit override_coding_run");
        ApiError::internal("failed to override coding run")
    })?;

    Ok(OverrideCodingRunResponse {
        id: coding_run_id,
        human_override: stored.to_string(),
    })
}
