use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::arena::service::{list_roster, list_trials};
use crate::arena::types::{
    CodingLeaderboard, CodingLeaderboardQuery, CodingModelProfile, CodingRunRecord,
    CodingSliceCell, HostBiasCell, Leaderboard, LeaderboardQuery, LensCell, ModelScorecard,
    RecentRunView, RosterEntry, RunCandidateView, RunLegView, SKILL_ADVERSARIAL_REVIEW,
    SKILL_PLAN_TOURNAMENT, TOURNAMENT_LENSES, Trial, TrialComparison, VALID_SIZE_BUCKETS,
    VALID_TASK_KINDS, infer_lineage, is_trial_competing_role, is_valid_skill, model_effort_key,
};
use crate::routes::ApiError;

#[derive(Debug, Clone, sqlx::FromRow)]
struct RunRow {
    id: i64,
    run_key: String,
    skill: String,
    artifact_kind: String,
    artifact_ref: String,
    repo: String,
    host_model: String,
    host_reasoning_effort: String,
    host_lineage: String,
    harness: String,
    machine: String,
    approach_verdict: String,
    winning_role: String,
    occurred_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct LegRow {
    run_id: i64,
    slot_index: i32,
    role: String,
    lineage: String,
    model: String,
    reasoning_effort: String,
    is_challenger: bool,
    trial_id: Option<i64>,
    status: String,
    duration_ms: i32,
    won_core_plan: bool,
    cherry_picks_count: i32,
    critic_adopted_count: i32,
    critic_bloat_dropped_count: i32,
}

#[derive(Debug, Clone)]
struct EnrichedLeg {
    row: LegRow,
    in_multi_leg_fanout: bool,
    is_straggler: bool,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct CandidateRow {
    id: i64,
    run_id: i64,
    candidate_index: i32,
    summary: String,
    severity: String,
    disposition: String,
    caused_approach_pivot: bool,
    proposing_models: Vec<String>,
    human_override: String,
    override_note: String,
}

impl CandidateRow {
    fn effective_disposition(&self) -> &str {
        match self.human_override.as_str() {
            "confirmed" => "adopted",
            "overturned" => "set_aside",
            _ => self.disposition.as_str(),
        }
    }
}

pub async fn get_leaderboard(
    pool: &PgPool,
    user_id: Uuid,
    query: LeaderboardQuery,
) -> Result<Leaderboard, ApiError> {
    let days = query.days.unwrap_or(90).clamp(1, 3650);
    let skill = query.skill.as_deref().map(str::trim).unwrap_or("");
    if !skill.is_empty() && !is_valid_skill(skill) {
        return Err(ApiError::bad_request(format!(
            "skill must be {SKILL_ADVERSARIAL_REVIEW} or {SKILL_PLAN_TOURNAMENT}"
        )));
    }
    let repo = query.repo.as_deref().map(str::trim).unwrap_or("");
    let since = query
        .from
        .unwrap_or_else(|| Utc::now() - Duration::days(i64::from(days)));
    let until = query.to;
    if let Some(to) = until
        && to < since
    {
        return Err(ApiError::bad_request("to must be >= from"));
    }

    let roster = list_roster(pool, user_id, Some(skill)).await?;
    let all_trials = list_trials(pool, user_id, None).await?;
    let trial_rows: Vec<Trial> = all_trials
        .into_iter()
        .filter(|t| skill.is_empty() || t.skill == skill)
        .collect();

    let mut fetch_since = since;
    for t in &trial_rows {
        let relevant_trial = matches!(t.status.as_str(), "active" | "completed")
            || t.decided_at.is_some_and(|decided| decided >= since);
        if relevant_trial && t.created_at < fetch_since {
            fetch_since = t.created_at;
        }
    }

    let all_runs = sqlx::query_as::<_, RunRow>(
        "SELECT id, run_key, skill, artifact_kind, artifact_ref, repo,
                host_model, host_reasoning_effort, host_lineage, harness, machine,
                approach_verdict, winning_role, occurred_at
         FROM arena_runs
         WHERE user_id = $1
           AND occurred_at >= $2
           AND ($3 = '' OR skill = $3)
           AND ($4 = '' OR lower(repo) = lower($4))
         ORDER BY occurred_at DESC, id DESC",
    )
    .bind(user_id)
    .bind(fetch_since)
    .bind(skill)
    .bind(repo)
    .fetch_all(pool)
    .await
    .map_err(|err| {
        tracing::error!(?err, "failed to query arena_runs for leaderboard");
        ApiError::internal("failed to load leaderboard")
    })?;

    let run_ids: Vec<i64> = all_runs.iter().map(|r| r.id).collect();
    let mut legs_by_run: HashMap<i64, Vec<EnrichedLeg>> = HashMap::new();
    let mut cands_by_run: HashMap<i64, Vec<CandidateRow>> = HashMap::new();

    if !run_ids.is_empty() {
        let raw_legs = sqlx::query_as::<_, LegRow>(
            "SELECT run_id, slot_index, role, lineage, model, reasoning_effort, is_challenger, trial_id,
                    status, duration_ms, won_core_plan, cherry_picks_count,
                    critic_adopted_count, critic_bloat_dropped_count
             FROM arena_run_legs
             WHERE run_id = ANY($1)
             ORDER BY run_id ASC, slot_index ASC",
        )
        .bind(&run_ids)
        .fetch_all(pool)
        .await
        .map_err(|err| {
            tracing::error!(?err, "failed to query arena_run_legs for leaderboard");
            ApiError::internal("failed to load leaderboard")
        })?;

        let run_skill_by_id: HashMap<i64, &str> =
            all_runs.iter().map(|r| (r.id, r.skill.as_str())).collect();

        let mut raw_by_run: HashMap<i64, Vec<LegRow>> = HashMap::new();
        for leg in raw_legs {
            raw_by_run.entry(leg.run_id).or_default().push(leg);
        }

        for (run_id, legs) in raw_by_run {
            let run_skill = run_skill_by_id.get(&run_id).copied().unwrap_or("");
            let timed_competing: Vec<i32> = legs
                .iter()
                .filter(|l| {
                    is_trial_competing_role(run_skill, &l.role)
                        && matches!(l.status.as_str(), "ok" | "retried")
                        && l.duration_ms > 0
                })
                .map(|l| l.duration_ms)
                .collect();

            let is_multi_leg = timed_competing.len() >= 2;
            let max_dur = timed_competing.iter().copied().max().unwrap_or(0);
            let min_dur = timed_competing.iter().copied().min().unwrap_or(0);

            let enriched = legs
                .into_iter()
                .map(|row| {
                    let is_timed_competing = is_trial_competing_role(run_skill, &row.role)
                        && matches!(row.status.as_str(), "ok" | "retried")
                        && row.duration_ms > 0;
                    let in_multi_leg_fanout = is_multi_leg && is_timed_competing;
                    let is_straggler =
                        in_multi_leg_fanout && max_dur > min_dur && row.duration_ms == max_dur;
                    EnrichedLeg {
                        row,
                        in_multi_leg_fanout,
                        is_straggler,
                    }
                })
                .collect();
            legs_by_run.insert(run_id, enriched);
        }

        let raw_cands = sqlx::query_as::<_, CandidateRow>(
            "SELECT id, run_id, candidate_index, summary, severity, disposition,
                    caused_approach_pivot, proposing_models, human_override, override_note
             FROM arena_candidates
             WHERE run_id = ANY($1)
             ORDER BY run_id ASC, candidate_index ASC",
        )
        .bind(&run_ids)
        .fetch_all(pool)
        .await
        .map_err(|err| {
            tracing::error!(?err, "failed to query arena_candidates for leaderboard");
            ApiError::internal("failed to load leaderboard")
        })?;

        for cand in raw_cands {
            cands_by_run.entry(cand.run_id).or_default().push(cand);
        }
    }

    let window_runs: Vec<RunRow> = all_runs
        .iter()
        .filter(|r| r.occurred_at >= since && until.is_none_or(|to| r.occurred_at <= to))
        .cloned()
        .collect();

    let recent_runs: Vec<RecentRunView> = window_runs
        .iter()
        .take(25)
        .map(|r| {
            let legs = legs_by_run
                .get(&r.id)
                .map(|ls| {
                    ls.iter()
                        .map(|el| RunLegView {
                            slot_index: el.row.slot_index,
                            role: el.row.role.clone(),
                            lineage: el.row.lineage.clone(),
                            model: el.row.model.clone(),
                            reasoning_effort: el.row.reasoning_effort.clone(),
                            is_challenger: el.row.is_challenger,
                            status: el.row.status.clone(),
                            duration_ms: el.row.duration_ms,
                            won_core_plan: el.row.won_core_plan,
                            cherry_picks_count: el.row.cherry_picks_count,
                            critic_adopted_count: el.row.critic_adopted_count,
                            critic_bloat_dropped_count: el.row.critic_bloat_dropped_count,
                        })
                        .collect()
                })
                .unwrap_or_default();

            let candidates = cands_by_run
                .get(&r.id)
                .map(|cs| {
                    cs.iter()
                        .map(|c| RunCandidateView {
                            id: c.id,
                            candidate_index: c.candidate_index,
                            summary: c.summary.clone(),
                            severity: c.severity.clone(),
                            disposition: c.disposition.clone(),
                            effective_disposition: c.effective_disposition().to_string(),
                            caused_approach_pivot: c.caused_approach_pivot,
                            proposing_models: c.proposing_models.clone(),
                            human_override: c.human_override.clone(),
                            override_note: c.override_note.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default();

            RecentRunView {
                id: r.id,
                run_key: r.run_key.clone(),
                skill: r.skill.clone(),
                artifact_kind: r.artifact_kind.clone(),
                artifact_ref: r.artifact_ref.clone(),
                repo: r.repo.clone(),
                host_model: r.host_model.clone(),
                host_reasoning_effort: r.host_reasoning_effort.clone(),
                harness: r.harness.clone(),
                machine: r.machine.clone(),
                approach_verdict: r.approach_verdict.clone(),
                winning_role: r.winning_role.clone(),
                occurred_at: r.occurred_at,
                legs,
                candidates,
            }
        })
        .collect();

    let (models, lens_matrix, host_bias_matrix) = aggregate_runs(
        &window_runs,
        &legs_by_run,
        &legs_by_run,
        &cands_by_run,
        &roster,
        &trial_rows,
    );

    let mut trials = Vec::with_capacity(trial_rows.len());
    for t in &trial_rows {
        let mut t_runs = Vec::new();
        let mut filtered_legs: HashMap<i64, Vec<EnrichedLeg>> = HashMap::new();

        for r in &all_runs {
            if r.skill != t.skill {
                continue;
            }
            if !t.allow_pr_diff && r.artifact_kind == "pr_diff" {
                continue;
            }
            if r.occurred_at < t.created_at {
                continue;
            }
            if let Some(decided_at) = t.decided_at
                && r.occurred_at > decided_at
            {
                continue;
            }

            let Some(run_legs) = legs_by_run.get(&r.id) else {
                continue;
            };
            let matching: Vec<EnrichedLeg> = run_legs
                .iter()
                .filter(|el| {
                    let l = &el.row;
                    let leg_key = model_effort_key(&l.model, &l.reasoning_effort);
                    l.trial_id == Some(t.id)
                        && l.lineage == t.lineage
                        && is_trial_competing_role(&t.skill, &l.role)
                        && ((l.is_challenger
                            && leg_key
                                == model_effort_key(
                                    &t.challenger_model,
                                    &t.challenger_reasoning_effort,
                                ))
                            || (!l.is_challenger
                                && leg_key
                                    == model_effort_key(
                                        &t.champion_model,
                                        &t.champion_reasoning_effort,
                                    )))
                })
                .cloned()
                .collect();

            if !matching.is_empty() {
                filtered_legs.insert(r.id, matching);
                t_runs.push(r.clone());
            }
        }

        let (t_models, _, _) = aggregate_runs(
            &t_runs,
            &filtered_legs,
            &legs_by_run,
            &cands_by_run,
            &roster,
            &[],
        );

        let mut champ_card = ModelScorecard {
            model: t.champion_model.clone(),
            reasoning_effort: t.champion_reasoning_effort.clone(),
            lineage: t.lineage.clone(),
            ..Default::default()
        };
        let mut chall_card = ModelScorecard {
            model: t.challenger_model.clone(),
            reasoning_effort: t.challenger_reasoning_effort.clone(),
            lineage: t.lineage.clone(),
            ..Default::default()
        };
        let champ_key = model_effort_key(&t.champion_model, &t.champion_reasoning_effort);
        let chall_key = model_effort_key(&t.challenger_model, &t.challenger_reasoning_effort);
        for m in t_models {
            let m_key = model_effort_key(&m.model, &m.reasoning_effort);
            if m_key == champ_key {
                champ_card = m;
            } else if m_key == chall_key {
                chall_card = m;
            }
        }

        let (verdict_hint, verdict_notes, p50_delta_ms, speedup_ratio) =
            compute_trial_verdict(t, &champ_card, &chall_card);

        trials.push(TrialComparison {
            trial: t.clone(),
            champion: champ_card,
            challenger: chall_card,
            verdict_hint,
            verdict_notes,
            p50_delta_ms,
            speedup_ratio,
        });
    }

    Ok(Leaderboard {
        window_days: days,
        total_runs: window_runs.len() as i32,
        roster,
        models,
        lens_matrix,
        host_bias_matrix,
        trials,
        recent_runs,
    })
}

#[derive(Default)]
struct ModelAccum {
    sc: ModelScorecard,
    duration_sum: i64,
    durations: Vec<i32>,
    multi_leg_timed_runs: i32,
    timed_review_duration_ms: i64,
    timed_review_catches: i32,
}

fn aggregate_runs(
    runs: &[RunRow],
    legs_by_run: &HashMap<i64, Vec<EnrichedLeg>>,
    full_legs_by_run: &HashMap<i64, Vec<EnrichedLeg>>,
    cands_by_run: &HashMap<i64, Vec<CandidateRow>>,
    roster: &[RosterEntry],
    trials: &[Trial],
) -> (Vec<ModelScorecard>, Vec<LensCell>, Vec<HostBiasCell>) {
    let mut champion_set: HashSet<(String, String)> = HashSet::new();
    let mut model_lineage: HashMap<String, String> = HashMap::new();
    for r in roster {
        let champ_model_lower = r.champion_model.to_ascii_lowercase();
        if r.enabled {
            champion_set.insert(model_effort_key(
                &r.champion_model,
                &r.champion_reasoning_effort,
            ));
        }
        model_lineage.insert(champ_model_lower, r.lineage.clone());
        if !r.backup_model.is_empty() {
            model_lineage.insert(r.backup_model.to_ascii_lowercase(), r.lineage.clone());
        }
    }

    let mut active_challenger: HashMap<(String, String), i64> = HashMap::new();
    for t in trials {
        if matches!(t.status.as_str(), "active" | "completed") {
            active_challenger.insert(
                model_effort_key(&t.challenger_model, &t.challenger_reasoning_effort),
                t.id,
            );
        }
    }

    let mut model_map: HashMap<(String, String), ModelAccum> = HashMap::new();
    let mut lens_map: HashMap<((String, String), String), LensCell> = HashMap::new();
    let mut bias_map = HashMap::new();

    let get_model = |model_map: &mut HashMap<(String, String), ModelAccum>,
                     model_lineage: &mut HashMap<String, String>,
                     model_name: &str,
                     reasoning_effort: &str,
                     lineage_hint: &str|
     -> Option<(String, String)> {
        let trimmed = model_name.trim();
        if trimmed.is_empty() {
            return None;
        }
        let key = model_effort_key(trimmed, reasoning_effort);
        let model_lower = key.0.clone();
        let effort = key.1.clone();
        let acc = model_map.entry(key.clone()).or_insert_with(|| {
            let lin = if !lineage_hint.is_empty() {
                lineage_hint.to_string()
            } else {
                model_lineage
                    .get(&model_lower)
                    .cloned()
                    .unwrap_or_else(|| infer_lineage(trimmed, roster))
            };
            ModelAccum {
                sc: ModelScorecard {
                    model: trimmed.to_string(),
                    reasoning_effort: effort,
                    lineage: lin,
                    is_current_champion: champion_set.contains(&key),
                    active_challenger_trial_id: active_challenger.get(&key).copied(),
                    ..Default::default()
                },
                ..Default::default()
            }
        });
        if acc.sc.lineage.is_empty() && !lineage_hint.is_empty() {
            acc.sc.lineage = lineage_hint.to_string();
        }
        if !acc.sc.lineage.is_empty() {
            model_lineage.insert(model_lower, acc.sc.lineage.clone());
        }
        Some(key)
    };

    for r in roster {
        let _ = get_model(
            &mut model_map,
            &mut model_lineage,
            &r.champion_model,
            &r.champion_reasoning_effort,
            &r.lineage,
        );
    }

    for r in runs {
        let legs = legs_by_run.get(&r.id).map(Vec::as_slice).unwrap_or(&[]);
        let mut review_seen_in_run: HashMap<String, (String, String)> = HashMap::new();
        let mut timed_reviewer_in_run: HashSet<(String, String)> = HashSet::new();

        for el in legs {
            let l = &el.row;
            if matches!(l.role.as_str(), "ponytail" | "explore_scout") {
                continue;
            }
            let Some(key) = get_model(
                &mut model_map,
                &mut model_lineage,
                &l.model,
                &l.reasoning_effort,
                &l.lineage,
            ) else {
                continue;
            };
            let acc = model_map.get_mut(&key).expect("model initialized");

            match l.status.as_str() {
                "fallback" | "timeout" => {
                    acc.sc.failures_count += 1;
                    continue;
                }
                "skipped" => continue,
                "retried" => {
                    acc.sc.retried_count += 1;
                }
                _ => {}
            }

            if l.duration_ms > 0 {
                acc.duration_sum += i64::from(l.duration_ms);
                acc.durations.push(l.duration_ms);
                if el.in_multi_leg_fanout {
                    acc.multi_leg_timed_runs += 1;
                }
                if el.is_straggler {
                    acc.sc.straggler_count += 1;
                }
            }

            if r.skill == SKILL_ADVERSARIAL_REVIEW && l.role == "reviewer" {
                let model_lower = key.0.clone();
                if review_seen_in_run
                    .insert(model_lower, key.clone())
                    .is_none()
                {
                    acc.sc.review_runs += 1;
                }
                if l.duration_ms > 0 {
                    acc.timed_review_duration_ms += i64::from(l.duration_ms);
                    timed_reviewer_in_run.insert(key);
                }
            } else if r.skill == SKILL_PLAN_TOURNAMENT {
                match l.role.as_str() {
                    "minimal_diff" | "clean_architecture" | "edge_cases" => {
                        acc.sc.planner_runs += 1;
                        let won = l.won_core_plan || r.winning_role == l.role;
                        if won {
                            acc.sc.core_plan_wins += 1;
                        }
                        acc.sc.cherry_picks += l.cherry_picks_count;

                        let cell = lens_map
                            .entry((key.clone(), l.role.clone()))
                            .or_insert_with(|| LensCell {
                                model: acc.sc.model.clone(),
                                reasoning_effort: acc.sc.reasoning_effort.clone(),
                                lens: l.role.clone(),
                                ..Default::default()
                            });
                        cell.runs += 1;
                        if won {
                            cell.wins += 1;
                        }
                        cell.cherry_picks += l.cherry_picks_count;
                    }
                    "critic" => {
                        acc.sc.critic_runs += 1;
                        acc.sc.critic_adopted += l.critic_adopted_count;
                        acc.sc.critic_bloat_dropped += l.critic_bloat_dropped_count;
                    }
                    _ => {}
                }
            }
        }

        if r.skill == SKILL_ADVERSARIAL_REVIEW {
            let full_legs = full_legs_by_run
                .get(&r.id)
                .map(Vec::as_slice)
                .unwrap_or(legs);
            let mut full_review_seen_in_run: HashSet<String> = HashSet::new();
            for el in full_legs {
                let l = &el.row;
                if l.role == "reviewer" && matches!(l.status.as_str(), "ok" | "retried") {
                    let trimmed = l.model.trim();
                    if !trimmed.is_empty() {
                        full_review_seen_in_run.insert(trimmed.to_ascii_lowercase());
                    }
                }
            }

            let cands = cands_by_run.get(&r.id).map(Vec::as_slice).unwrap_or(&[]);
            for c in cands {
                let mut all_panel_proposers: Vec<String> = Vec::new();
                let mut seen_p: HashSet<String> = HashSet::new();
                for pm in &c.proposing_models {
                    let pt = pm.trim();
                    let pl = pt.to_ascii_lowercase();
                    if pt.is_empty() || pl == "ponytail" || !seen_p.insert(pl.clone()) {
                        continue;
                    }
                    if !full_review_seen_in_run.contains(&pl) {
                        continue;
                    }
                    all_panel_proposers.push(pt.to_string());
                }
                if all_panel_proposers.is_empty() {
                    continue;
                }

                let eff = c.effective_disposition();
                let is_solo = all_panel_proposers.len() == 1;

                for pm in &all_panel_proposers {
                    let pl = pm.to_ascii_lowercase();
                    let Some(key) = review_seen_in_run.get(&pl).cloned() else {
                        continue;
                    };
                    let Some(acc) = model_map.get_mut(&key) else {
                        continue;
                    };
                    match eff {
                        "adopted" => {
                            if is_solo {
                                acc.sc.solo_catches += 1;
                            } else {
                                acc.sc.shared_catches += 1;
                            }
                            if c.caused_approach_pivot {
                                acc.sc.approach_pivots += 1;
                            }
                            if timed_reviewer_in_run.contains(&key) {
                                acc.timed_review_catches += 1;
                            }
                        }
                        "set_aside" => {
                            acc.sc.set_aside_count += 1;
                        }
                        "open_question" => {
                            acc.sc.open_question_count += 1;
                        }
                        _ => {}
                    }

                    if !r.host_model.is_empty() && matches!(eff, "adopted" | "set_aside") {
                        let reviewer_model = acc.sc.model.clone();
                        let reviewer_effort = acc.sc.reasoning_effort.clone();
                        let reviewer_lineage = acc.sc.lineage.clone();
                        let host_lin = if !r.host_lineage.is_empty() {
                            r.host_lineage.clone()
                        } else {
                            infer_lineage(&r.host_model, roster)
                        };
                        let bkey = (
                            model_effort_key(&r.host_model, &r.host_reasoning_effort),
                            model_effort_key(&reviewer_model, &reviewer_effort),
                        );
                        let bc = bias_map.entry(bkey).or_insert_with(|| HostBiasCell {
                            host_model: r.host_model.clone(),
                            host_reasoning_effort: r.host_reasoning_effort.clone(),
                            host_lineage: host_lin,
                            reviewer_model,
                            reviewer_reasoning_effort: reviewer_effort,
                            reviewer_lineage,
                            ..Default::default()
                        });
                        bc.candidates_evaluated += 1;
                        if eff == "adopted" {
                            bc.adopted_count += 1;
                        }
                    }
                }
            }
        }
    }

    let mut lens_cells: Vec<LensCell> = lens_map
        .into_values()
        .map(|mut cell| {
            if cell.runs > 0 {
                cell.win_rate = round3(f64::from(cell.wins) / f64::from(cell.runs));
            }
            cell
        })
        .collect();
    lens_cells.sort_by(|a, b| {
        a.model
            .cmp(&b.model)
            .then_with(|| a.reasoning_effort.cmp(&b.reasoning_effort))
            .then_with(|| a.lens.cmp(&b.lens))
    });

    let mut model_lens_lookup: HashMap<((String, String), String), LensCell> = HashMap::new();
    for cell in &lens_cells {
        model_lens_lookup.insert(
            (
                model_effort_key(&cell.model, &cell.reasoning_effort),
                cell.lens.clone(),
            ),
            cell.clone(),
        );
    }

    let mut models: Vec<ModelScorecard> = model_map
        .into_iter()
        .map(|(key, mut acc)| {
            acc.durations.sort_unstable();
            acc.sc.timed_runs = acc.durations.len() as i32;
            if !acc.durations.is_empty() {
                acc.sc.avg_duration_ms = (acc.duration_sum / (acc.durations.len() as i64)) as i32;
                acc.sc.p50_duration_ms = median_ms(&acc.durations);
                acc.sc.p90_duration_ms = percentile_ms(&acc.durations, 90);
            }
            if acc.multi_leg_timed_runs > 0 {
                acc.sc.straggler_rate =
                    round3(f64::from(acc.sc.straggler_count) / f64::from(acc.multi_leg_timed_runs));
            }
            if acc.timed_review_duration_ms > 0 {
                let minutes = (acc.timed_review_duration_ms as f64) / 60_000.0;
                acc.sc.catches_per_minute = round3(f64::from(acc.timed_review_catches) / minutes);
            }

            let adopted = acc.sc.solo_catches + acc.sc.shared_catches;
            let denom = adopted + acc.sc.set_aside_count;
            if denom > 0 {
                acc.sc.precision = round3(f64::from(adopted) / f64::from(denom));
            }
            if acc.sc.review_runs > 0 {
                acc.sc.solo_catch_rate =
                    round3(f64::from(acc.sc.solo_catches) / f64::from(acc.sc.review_runs));
            }

            let mut lens_score_sum = 0.0;
            let mut lenses_observed = 0;
            for lens in TOURNAMENT_LENSES {
                if let Some(cell) = model_lens_lookup.get(&(key.clone(), lens.to_string()))
                    && cell.runs > 0
                {
                    let pick_bonus =
                        (f64::from(cell.cherry_picks) / f64::from(cell.runs)).min(3.0) * 0.15;
                    lens_score_sum += cell.win_rate + pick_bonus;
                    lenses_observed += 1;
                }
            }
            if lenses_observed > 0 {
                acc.sc.debiased_tournament_score =
                    round3(lens_score_sum / f64::from(lenses_observed));
            }
            acc.sc
        })
        .collect();

    models.sort_by(|a, b| {
        b.is_current_champion
            .cmp(&a.is_current_champion)
            .then_with(|| {
                let score_a = a.solo_catch_rate + a.debiased_tournament_score;
                let score_b = b.solo_catch_rate + b.debiased_tournament_score;
                score_b
                    .partial_cmp(&score_a)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.model.cmp(&b.model))
            .then_with(|| a.reasoning_effort.cmp(&b.reasoning_effort))
    });

    let mut bias_cells: Vec<HostBiasCell> = bias_map
        .into_values()
        .map(|mut bc| {
            if bc.candidates_evaluated > 0 {
                bc.adoption_rate =
                    round3(f64::from(bc.adopted_count) / f64::from(bc.candidates_evaluated));
            }
            bc
        })
        .collect();
    bias_cells.sort_by(|a, b| {
        a.host_model
            .cmp(&b.host_model)
            .then_with(|| a.host_reasoning_effort.cmp(&b.host_reasoning_effort))
            .then_with(|| a.reviewer_model.cmp(&b.reviewer_model))
            .then_with(|| {
                a.reviewer_reasoning_effort
                    .cmp(&b.reviewer_reasoning_effort)
            })
    });

    (models, lens_cells, bias_cells)
}

pub(crate) fn compute_trial_verdict(
    t: &Trial,
    champ: &ModelScorecard,
    chall: &ModelScorecard,
) -> (String, String, i32, f64) {
    let (p50_delta_ms, speedup_ratio) = if champ.p50_duration_ms > 0 && chall.p50_duration_ms > 0 {
        (
            chall.p50_duration_ms - champ.p50_duration_ms,
            round3(f64::from(champ.p50_duration_ms) / f64::from(chall.p50_duration_ms)),
        )
    } else {
        (0, 0.0)
    };

    let min_champ_runs = (t.target_runs / 2).clamp(1, 3);
    let scored_chall_runs = if t.skill == SKILL_ADVERSARIAL_REVIEW {
        chall.review_runs
    } else {
        chall.planner_runs
    };
    if t.completed_runs < t.target_runs || scored_chall_runs < t.target_runs {
        return (
            "insufficient_data".to_string(),
            format!(
                "{} / {} challenger runs recorded",
                scored_chall_runs.min(t.completed_runs),
                t.target_runs
            ),
            p50_delta_ms,
            speedup_ratio,
        );
    }

    let has_timing = champ.p50_duration_ms > 0 && chall.p50_duration_ms > 0;
    let materially_faster =
        has_timing && f64::from(chall.p50_duration_ms) <= f64::from(champ.p50_duration_ms) * 0.75;
    let materially_slower =
        has_timing && f64::from(chall.p50_duration_ms) >= f64::from(champ.p50_duration_ms) * 1.50;

    if t.skill == SKILL_ADVERSARIAL_REVIEW {
        if champ.review_runs < min_champ_runs {
            return (
                "insufficient_data".to_string(),
                format!(
                    "incumbent has {} / {} baseline review runs in this trial",
                    champ.review_runs, min_champ_runs
                ),
                p50_delta_ms,
                speedup_ratio,
            );
        }
        let rate_delta = chall.solo_catch_rate - champ.solo_catch_rate;
        let champ_has_prec =
            (champ.solo_catches + champ.shared_catches + champ.set_aside_count) > 0;
        let chall_has_prec =
            (chall.solo_catches + chall.shared_catches + chall.set_aside_count) > 0;
        let prec_delta = if champ_has_prec && chall_has_prec {
            chall.precision - champ.precision
        } else {
            0.0
        };
        if (rate_delta >= 0.05 && prec_delta >= -0.05) || (rate_delta >= 0.0 && prec_delta >= 0.08)
        {
            if materially_slower && rate_delta < 0.15 && prec_delta < 0.15 {
                return (
                    "close_call".to_string(),
                    format!(
                        "Challenger improves solo catch rate {:+.1}% and precision {:+.1}%, but p50 latency is {:+}ms slower ({}ms vs {}ms)",
                        rate_delta * 100.0,
                        prec_delta * 100.0,
                        p50_delta_ms,
                        chall.p50_duration_ms,
                        champ.p50_duration_ms
                    ),
                    p50_delta_ms,
                    speedup_ratio,
                );
            }
            return (
                "promote".to_string(),
                format!(
                    "Challenger solo catch rate {:+.1}% and precision {:+.1}% vs champion",
                    rate_delta * 100.0,
                    prec_delta * 100.0
                ),
                p50_delta_ms,
                speedup_ratio,
            );
        }
        if rate_delta <= -0.05 || prec_delta <= -0.10 {
            return (
                "retire".to_string(),
                format!(
                    "Challenger trails champion (solo catch rate {:+.1}%, precision {:+.1}%)",
                    rate_delta * 100.0,
                    prec_delta * 100.0
                ),
                p50_delta_ms,
                speedup_ratio,
            );
        }
        if rate_delta >= -0.02
            && prec_delta >= -0.03
            && materially_faster
            && chall.straggler_rate <= champ.straggler_rate + 0.05
            && chall.failures_count <= champ.failures_count
        {
            return (
                "promote".to_string(),
                format!(
                    "Challenger matches quality (solo catch rate {:+.1}%, precision {:+.1}%) with {:.2}x faster p50 latency ({}ms vs {}ms)",
                    rate_delta * 100.0,
                    prec_delta * 100.0,
                    speedup_ratio,
                    chall.p50_duration_ms,
                    champ.p50_duration_ms
                ),
                p50_delta_ms,
                speedup_ratio,
            );
        }
        if materially_slower && rate_delta <= 0.02 && prec_delta <= 0.03 {
            return (
                "retire".to_string(),
                format!(
                    "Challenger fails to beat quality (solo catch rate {:+.1}%, precision {:+.1}%) while p50 latency is {:+}ms slower ({}ms vs {}ms)",
                    rate_delta * 100.0,
                    prec_delta * 100.0,
                    p50_delta_ms,
                    chall.p50_duration_ms,
                    champ.p50_duration_ms
                ),
                p50_delta_ms,
                speedup_ratio,
            );
        }
        return (
            "close_call".to_string(),
            format!(
                "Within tie-band (solo catch rate {:+.1}%, precision {:+.1}%)",
                rate_delta * 100.0,
                prec_delta * 100.0
            ),
            p50_delta_ms,
            speedup_ratio,
        );
    }

    if champ.planner_runs < min_champ_runs {
        return (
            "insufficient_data".to_string(),
            format!(
                "incumbent has {} / {} baseline tournament runs in this trial",
                champ.planner_runs, min_champ_runs
            ),
            p50_delta_ms,
            speedup_ratio,
        );
    }
    let score_delta = chall.debiased_tournament_score - champ.debiased_tournament_score;
    if score_delta >= 0.08 {
        if materially_slower && score_delta < 0.20 {
            return (
                "close_call".to_string(),
                format!(
                    "Challenger de-biased tournament score {:+.3} vs champion, but p50 latency is {:+}ms slower ({}ms vs {}ms)",
                    score_delta, p50_delta_ms, chall.p50_duration_ms, champ.p50_duration_ms
                ),
                p50_delta_ms,
                speedup_ratio,
            );
        }
        return (
            "promote".to_string(),
            format!(
                "Challenger de-biased tournament score {:+.3} vs champion",
                score_delta
            ),
            p50_delta_ms,
            speedup_ratio,
        );
    }
    if score_delta <= -0.08 {
        return (
            "retire".to_string(),
            format!(
                "Challenger de-biased tournament score {:+.3} vs champion",
                score_delta
            ),
            p50_delta_ms,
            speedup_ratio,
        );
    }
    if score_delta >= -0.02
        && materially_faster
        && chall.straggler_rate <= champ.straggler_rate + 0.05
        && chall.failures_count <= champ.failures_count
    {
        return (
            "promote".to_string(),
            format!(
                "Challenger matches tournament quality ({:+.3}) with {:.2}x faster p50 latency ({}ms vs {}ms)",
                score_delta, speedup_ratio, chall.p50_duration_ms, champ.p50_duration_ms
            ),
            p50_delta_ms,
            speedup_ratio,
        );
    }
    if materially_slower && score_delta <= 0.02 {
        return (
            "retire".to_string(),
            format!(
                "Challenger fails to beat tournament quality ({:+.3}) while p50 latency is {:+}ms slower ({}ms vs {}ms)",
                score_delta, p50_delta_ms, chall.p50_duration_ms, champ.p50_duration_ms
            ),
            p50_delta_ms,
            speedup_ratio,
        );
    }
    (
        "close_call".to_string(),
        format!(
            "De-biased tournament score delta {:+.3} is within tie-band",
            score_delta
        ),
        p50_delta_ms,
        speedup_ratio,
    )
}

pub(crate) fn median_ms(sorted: &[i32]) -> i32 {
    if sorted.is_empty() {
        return 0;
    }
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        ((i64::from(sorted[n / 2 - 1]) + i64::from(sorted[n / 2])) / 2) as i32
    }
}

pub(crate) fn percentile_ms(sorted: &[i32], pct: usize) -> i32 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (sorted.len() * pct).div_ceil(100);
    let idx = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[idx]
}

pub(crate) fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

fn effective_coding_outcomes(r: &CodingRunRecord) -> (bool, bool, bool) {
    match r.human_override.as_str() {
        "overturned" => (true, false, false),
        "confirmed" => (false, r.first_pass_clean && !r.abandoned, !r.abandoned),
        _ => (
            r.escaped_regression,
            r.first_pass_clean && !r.escaped_regression && !r.abandoned,
            r.merged_without_followup_24h && !r.escaped_regression && !r.abandoned,
        ),
    }
}

pub async fn get_coding_leaderboard(
    pool: &PgPool,
    user_id: Uuid,
    query: CodingLeaderboardQuery,
) -> Result<CodingLeaderboard, ApiError> {
    let days = query.days.unwrap_or(90);
    if !(1..=3650).contains(&days) {
        return Err(ApiError::bad_request("days must be between 1 and 3650"));
    }

    let task_kind_filter = query
        .task_kind
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .to_ascii_lowercase();
    if !task_kind_filter.is_empty() && !VALID_TASK_KINDS.contains(&task_kind_filter.as_str()) {
        return Err(ApiError::bad_request(format!(
            "invalid taskKind {:?}: must be one of {:?}",
            task_kind_filter, VALID_TASK_KINDS
        )));
    }

    let size_bucket_filter = query
        .size_bucket
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .to_ascii_uppercase();
    if !size_bucket_filter.is_empty() && !VALID_SIZE_BUCKETS.contains(&size_bucket_filter.as_str())
    {
        return Err(ApiError::bad_request(format!(
            "invalid sizeBucket {:?}: must be one of {:?}",
            size_bucket_filter, VALID_SIZE_BUCKETS
        )));
    }

    let repo_filter = query.repo.as_deref().map(str::trim).unwrap_or("");

    let now = Utc::now();
    let window_until = query.to.unwrap_or(now);
    let window_since = query
        .from
        .unwrap_or_else(|| window_until - Duration::days(i64::from(days)));
    if window_until <= window_since {
        return Err(ApiError::bad_request("to must be after from"));
    }
    let effective_window_days = if query.from.is_some() || query.to.is_some() {
        (((window_until - window_since).num_seconds() as f64) / 86_400.0)
            .ceil()
            .clamp(1.0, 3650.0) as i32
    } else {
        days
    };

    let roster = list_roster(pool, user_id, None).await?;

    let mut qb = sqlx::QueryBuilder::new(
        "SELECT id, run_key, model, reasoning_effort, lineage, harness, repo, pr_number, issue_ref,
                task_kind, loc_added, loc_deleted, size_bucket, files_changed, tests_delta,
                prompt_turn_count, red_ci_pushes, ccr_rounds, ccr_actionable_count, ccr_noise_count,
                human_threads_count, escaped_regression, merged_without_followup_24h, first_pass_clean,
                abandoned, time_to_green_sec, time_to_merge_sec, digest_summary,
                human_override, override_note, occurred_at, recorded_at
         FROM arena_coding_runs
         WHERE user_id = ",
    );
    qb.push_bind(user_id);
    qb.push(" AND occurred_at >= ");
    qb.push_bind(window_since);
    qb.push(" AND occurred_at <= ");
    qb.push_bind(window_until);

    if !repo_filter.is_empty() {
        qb.push(" AND repo = ");
        qb.push_bind(repo_filter);
    }
    if !task_kind_filter.is_empty() {
        qb.push(" AND task_kind = ");
        qb.push_bind(&task_kind_filter);
    }
    if !size_bucket_filter.is_empty() {
        qb.push(" AND size_bucket = ");
        qb.push_bind(&size_bucket_filter);
    }
    qb.push(" ORDER BY occurred_at DESC, id DESC");

    let runs: Vec<CodingRunRecord> = qb.build_query_as().fetch_all(pool).await.map_err(|err| {
        tracing::error!(?err, "failed to query coding runs for leaderboard");
        ApiError::internal("failed to load coding leaderboard")
    })?;

    #[derive(Default)]
    struct ModelAccum {
        display_model: String,
        lineage: String,
        total_runs: i32,
        merged_runs: i32,
        abandoned_runs: i32,
        zero_touch_runs: i32,
        first_pass_clean_runs: i32,
        merged_clean_24h_runs: i32,
        escaped_regressions: i32,
        total_prompt_turns: i64,
        total_red_ci_pushes: i64,
        total_ccr_rounds: i64,
        total_ccr_actionable: i64,
        total_ccr_noise: i64,
        total_human_threads: i64,
        total_loc_churn: i64,
        total_tests_delta: i64,
        green_secs: Vec<i32>,
        merge_secs: Vec<i32>,
    }

    #[derive(Default)]
    struct SliceAccum {
        display_model: String,
        runs: i32,
        zero_touch_runs: i32,
        first_pass_clean_runs: i32,
        escaped_regressions: i32,
        total_ccr_rounds: i64,
        merge_secs: Vec<i32>,
    }

    let mut by_model: HashMap<(String, String), ModelAccum> = HashMap::new();
    let mut by_slice: HashMap<((String, String), String, String), SliceAccum> = HashMap::new();

    for r in &runs {
        let (eff_escaped, eff_first_pass_clean, eff_merged_clean_24h) =
            effective_coding_outcomes(r);
        let is_zero_touch = !r.abandoned && !eff_escaped && r.prompt_turn_count <= 1;

        let mk = model_effort_key(&r.model, &r.reasoning_effort);
        let m = by_model.entry(mk.clone()).or_default();
        if m.display_model.is_empty() {
            m.display_model = r.model.clone();
        }
        if m.lineage.is_empty() {
            m.lineage = if r.lineage.is_empty() {
                infer_lineage(&r.model, &roster)
            } else {
                r.lineage.clone()
            };
        }
        m.total_runs += 1;
        if r.abandoned {
            m.abandoned_runs += 1;
        } else {
            m.merged_runs += 1;
        }
        if is_zero_touch {
            m.zero_touch_runs += 1;
        }
        if eff_first_pass_clean {
            m.first_pass_clean_runs += 1;
        }
        if eff_merged_clean_24h {
            m.merged_clean_24h_runs += 1;
        }
        if eff_escaped {
            m.escaped_regressions += 1;
        }
        m.total_prompt_turns += i64::from(r.prompt_turn_count);
        m.total_red_ci_pushes += i64::from(r.red_ci_pushes);
        m.total_ccr_rounds += i64::from(r.ccr_rounds);
        m.total_ccr_actionable += i64::from(r.ccr_actionable_count);
        m.total_ccr_noise += i64::from(r.ccr_noise_count);
        m.total_human_threads += i64::from(r.human_threads_count);
        m.total_loc_churn += i64::from(r.loc_added) + i64::from(r.loc_deleted);
        m.total_tests_delta += i64::from(r.tests_delta);
        if r.time_to_green_sec > 0 {
            m.green_secs.push(r.time_to_green_sec);
        }
        if !r.abandoned && r.time_to_merge_sec > 0 {
            m.merge_secs.push(r.time_to_merge_sec);
        }

        let s = by_slice
            .entry((mk, r.size_bucket.clone(), r.task_kind.clone()))
            .or_default();
        if s.display_model.is_empty() {
            s.display_model = r.model.clone();
        }
        s.runs += 1;
        if is_zero_touch {
            s.zero_touch_runs += 1;
        }
        if eff_first_pass_clean {
            s.first_pass_clean_runs += 1;
        }
        if eff_escaped {
            s.escaped_regressions += 1;
        }
        s.total_ccr_rounds += i64::from(r.ccr_rounds);
        if !r.abandoned && r.time_to_merge_sec > 0 {
            s.merge_secs.push(r.time_to_merge_sec);
        }
    }

    let mut models: Vec<CodingModelProfile> = by_model
        .into_iter()
        .map(|((_, reasoning_effort), mut acc)| {
            acc.green_secs.sort_unstable();
            acc.merge_secs.sort_unstable();
            let n = f64::from(acc.total_runs);
            let total_ccr_comments = acc.total_ccr_actionable + acc.total_ccr_noise;
            let ccr_noise_ratio = if total_ccr_comments > 0 {
                round3((acc.total_ccr_noise as f64) / (total_ccr_comments as f64))
            } else {
                0.0
            };

            CodingModelProfile {
                model: acc.display_model,
                reasoning_effort,
                lineage: acc.lineage,
                total_runs: acc.total_runs,
                merged_runs: acc.merged_runs,
                abandoned_runs: acc.abandoned_runs,
                zero_touch_runs: acc.zero_touch_runs,
                zero_touch_rate: round3(f64::from(acc.zero_touch_runs) / n),
                first_pass_clean_runs: acc.first_pass_clean_runs,
                first_pass_clean_rate: round3(f64::from(acc.first_pass_clean_runs) / n),
                merged_clean_24h_runs: acc.merged_clean_24h_runs,
                merged_clean_24h_rate: round3(f64::from(acc.merged_clean_24h_runs) / n),
                escaped_regressions: acc.escaped_regressions,
                escaped_regression_rate: round3(f64::from(acc.escaped_regressions) / n),
                avg_prompt_turns: round3(acc.total_prompt_turns as f64 / n),
                avg_red_ci_pushes: round3(acc.total_red_ci_pushes as f64 / n),
                avg_ccr_rounds: round3(acc.total_ccr_rounds as f64 / n),
                total_ccr_actionable: acc.total_ccr_actionable.clamp(0, i64::from(i32::MAX)) as i32,
                total_ccr_noise: acc.total_ccr_noise.clamp(0, i64::from(i32::MAX)) as i32,
                ccr_noise_ratio,
                total_human_threads: acc.total_human_threads.clamp(0, i64::from(i32::MAX)) as i32,
                avg_human_threads: round3(acc.total_human_threads as f64 / n),
                avg_loc_churn: (acc.total_loc_churn / i64::from(acc.total_runs))
                    .clamp(0, i64::from(i32::MAX)) as i32,
                avg_tests_delta: round3(acc.total_tests_delta as f64 / n),
                p50_time_to_green_sec: median_ms(&acc.green_secs),
                p90_time_to_green_sec: percentile_ms(&acc.green_secs, 90),
                p50_time_to_merge_sec: median_ms(&acc.merge_secs),
                p90_time_to_merge_sec: percentile_ms(&acc.merge_secs, 90),
            }
        })
        .collect();

    models.sort_by(|a, b| {
        b.merged_clean_24h_rate
            .total_cmp(&a.merged_clean_24h_rate)
            .then_with(|| b.zero_touch_rate.total_cmp(&a.zero_touch_rate))
            .then_with(|| b.first_pass_clean_rate.total_cmp(&a.first_pass_clean_rate))
            .then_with(|| b.total_runs.cmp(&a.total_runs))
            .then_with(|| a.model.cmp(&b.model))
            .then_with(|| a.reasoning_effort.cmp(&b.reasoning_effort))
    });

    let mut slices: Vec<CodingSliceCell> = by_slice
        .into_iter()
        .map(
            |(((_, reasoning_effort), size_bucket, task_kind), mut acc)| {
                acc.merge_secs.sort_unstable();
                let n = f64::from(acc.runs);
                CodingSliceCell {
                    model: acc.display_model,
                    reasoning_effort,
                    size_bucket,
                    task_kind,
                    runs: acc.runs,
                    zero_touch_rate: round3(f64::from(acc.zero_touch_runs) / n),
                    first_pass_clean_rate: round3(f64::from(acc.first_pass_clean_runs) / n),
                    escaped_regression_rate: round3(f64::from(acc.escaped_regressions) / n),
                    avg_ccr_rounds: round3(acc.total_ccr_rounds as f64 / n),
                    p50_time_to_merge_sec: median_ms(&acc.merge_secs),
                }
            },
        )
        .collect();

    slices.sort_by(|a, b| {
        a.model
            .cmp(&b.model)
            .then_with(|| a.reasoning_effort.cmp(&b.reasoning_effort))
            .then_with(|| a.size_bucket.cmp(&b.size_bucket))
            .then_with(|| a.task_kind.cmp(&b.task_kind))
    });

    let total_runs = runs.len() as i32;
    let recent_runs = runs.into_iter().take(25).collect();

    Ok(CodingLeaderboard {
        window_days: effective_window_days,
        total_runs,
        models,
        slices,
        recent_runs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_and_p90_percentiles_compute_accurately() {
        assert_eq!(median_ms(&[]), 0);
        assert_eq!(percentile_ms(&[], 90), 0);

        assert_eq!(median_ms(&[12_000]), 12_000);
        assert_eq!(percentile_ms(&[12_000], 90), 12_000);

        let even = [10_000, 20_000, 30_000, 80_000];
        assert_eq!(median_ms(&even), 25_000);
        assert_eq!(percentile_ms(&even, 90), 80_000);

        let ten = [10, 20, 30, 40, 50, 60, 70, 80, 90, 100];
        assert_eq!(median_ms(&ten), 55);
        assert_eq!(percentile_ms(&ten, 90), 90);
    }
}
