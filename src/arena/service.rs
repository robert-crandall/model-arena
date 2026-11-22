use std::collections::HashMap;

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::arena::types::{
    DecideTrialInput, PanelLeg, PanelSelection, RosterEntry, RosterSlotInput,
    SKILL_ADVERSARIAL_REVIEW, SKILL_PLAN_TOURNAMENT, SelectPanelInput, StartTrialInput,
    TOURNAMENT_LENSES, Trial, infer_lineage, is_valid_artifact_kind, is_valid_skill,
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
        "SELECT skill, lineage, champion_model, backup_model, sort_order, enabled, updated_at
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
        slot.backup_model = slot.backup_model.trim().to_string();
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
        "SELECT id, lineage, champion_model
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
            || !next_slot
                .champion_model
                .eq_ignore_ascii_case(&trial.champion_model)
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
            "INSERT INTO arena_roster (user_id, skill, lineage, champion_model, backup_model, sort_order, enabled, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, NOW())",
        )
        .bind(user_id)
        .bind(skill)
        .bind(&slot.lineage)
        .bind(&slot.champion_model)
        .bind(&slot.backup_model)
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
    enabled: bool,
}

#[derive(sqlx::FromRow)]
struct TrialRow {
    id: i64,
    skill: String,
    lineage: String,
    champion_model: String,
    challenger_model: String,
    target_runs: i32,
    completed_runs: i32,
    swap_percent: i32,
    allow_pr_diff: bool,
    status: String,
    created_at: DateTime<Utc>,
    decided_at: Option<DateTime<Utc>>,
}

impl From<TrialRow> for Trial {
    fn from(r: TrialRow) -> Self {
        Self {
            id: r.id,
            skill: r.skill,
            lineage: r.lineage,
            champion_model: r.champion_model,
            challenger_model: r.challenger_model,
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
        "SELECT champion_model, enabled
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
    if champ.champion_model.eq_ignore_ascii_case(challenger) {
        return Err(ApiError::bad_request(format!(
            "challenger {challenger:?} is already the champion"
        )));
    }

    let row = sqlx::query_as::<_, TrialRow>(
        "INSERT INTO arena_trials (
            user_id, skill, lineage, champion_model, challenger_model,
            target_runs, swap_percent, allow_pr_diff
         )
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         RETURNING
            id, skill, lineage, champion_model, challenger_model,
            target_runs, 0::INT AS completed_runs, swap_percent, allow_pr_diff,
            status, created_at, decided_at",
    )
    .bind(user_id)
    .bind(skill)
    .bind(&lineage)
    .bind(&champ.champion_model)
    .bind(challenger)
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
            t.id, t.skill, t.lineage, t.champion_model, t.challenger_model,
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
            id, skill, lineage, champion_model, challenger_model,
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
                 champion_model = $4,
                 updated_at = NOW()
             WHERE user_id = $1 AND skill = $2 AND lineage = $3
               AND lower(champion_model) = lower($5)",
        )
        .bind(user_id)
        .bind(&trial.skill)
        .bind(&trial.lineage)
        .bind(&trial.challenger_model)
        .bind(&trial.champion_model)
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

    let completed_runs: i32 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT l.run_id)::INT
         FROM arena_run_legs l
         JOIN arena_runs r ON r.id = l.run_id
         WHERE l.trial_id = $1
           AND r.user_id = $2
           AND r.skill = $3
           AND l.lineage = $4
           AND l.is_challenger = TRUE
           AND lower(l.model) = lower($5)
           AND (
               ($3 = 'adversarial-review' AND l.role = 'reviewer')
               OR ($3 = 'plan-tournament' AND l.role IN ('minimal_diff', 'clean_architecture', 'edge_cases'))
           )
           AND ($6 = TRUE OR r.artifact_kind <> 'pr_diff')
           AND r.occurred_at >= $7
           AND ($8::timestamptz IS NULL OR r.occurred_at <= $8)
           AND l.status IN ('ok', 'retried')",
    )
    .bind(trial_id)
    .bind(user_id)
    .bind(&trial.skill)
    .bind(&trial.lineage)
    .bind(&trial.challenger_model)
    .bind(trial.allow_pr_diff)
    .bind(trial.created_at)
    .bind(trial.decided_at)
    .fetch_one(&mut *tx)
    .await
    .map_err(|err| {
        tracing::error!(?err, "failed to count trial runs");
        ApiError::internal("failed to decide trial")
    })?;

    trial.completed_runs = completed_runs;

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
    swap_percent: i32,
    allow_pr_diff: bool,
    eligible_dispatches: i32,
    lens_counters: sqlx::types::Json<HashMap<String, i32>>,
}

struct ActiveTrialState {
    id: i64,
    challenger: String,
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
) -> Result<(String, bool, Option<i64>), ApiError> {
    let Some(at) = trials_by_lineage.get_mut(&entry.lineage) else {
        return Ok((entry.champion_model.clone(), false, None));
    };
    if artifact_kind == "pr_diff" && !at.allow_pr {
        return Ok((entry.champion_model.clone(), false, None));
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
        Ok((at.challenger.clone(), true, Some(at.id)))
    } else {
        Ok((entry.champion_model.clone(), false, None))
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
        "SELECT skill, lineage, champion_model, backup_model, sort_order, enabled, updated_at
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
        "SELECT id, lineage, challenger_model, swap_percent, allow_pr_diff, eligible_dispatches, lens_counters
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
            let (model, is_challenger, trial_id) = resolve_trial_model(
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
                backup_model: entry.backup_model.clone(),
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
            let (model, is_challenger, trial_id) =
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
                backup_model: entry.backup_model.clone(),
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
