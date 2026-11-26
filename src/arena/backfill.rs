use std::collections::HashMap;
use std::env;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::apiclient::ApiClient;
use crate::arena::service::validate_and_normalize_record_run_input;
use crate::arena::spool::{SpoolScope, submit_or_spool_run};
use crate::arena::types::{
    CandidateInput, RecordRunInput, RunLegInput, SKILL_ADVERSARIAL_REVIEW, SKILL_PLAN_TOURNAMENT,
    TOURNAMENT_LENSES, infer_lineage, normalize_reasoning_effort,
};

fn normalize_effort_lossy(raw: Option<&str>) -> String {
    raw.and_then(|s| normalize_reasoning_effort(s).ok())
        .unwrap_or_default()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BackfillArgs {
    /// Directory containing session folders with events.jsonl (defaults to ~/.copilot/session-state).
    #[serde(default)]
    pub session_dir: Option<String>,
    /// Only scan sessions modified within the last N days (defaults to 60).
    #[serde(default)]
    pub days: Option<i32>,
    /// Parse and count historical runs without submitting them to the server.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BackfillSummary {
    pub sessions_scanned: usize,
    pub runs_found: usize,
    pub runs_submitted: usize,
    pub sample_run_keys: Vec<String>,
}

fn is_shadow_bout_session(session_dir: &Path) -> bool {
    let Ok(raw) = fs::read_to_string(session_dir.join("workspace.yaml")) else {
        return false;
    };
    raw.lines().any(|line| {
        let Some(rest) = line.trim().strip_prefix("name:") else {
            return false;
        };
        let val = rest.trim().trim_matches(|c| c == '"' || c == '\'');
        val.starts_with("shadow-bout:")
    })
}

pub async fn run_backfill(
    client: &ApiClient,
    spool_scope: &SpoolScope,
    args: BackfillArgs,
) -> Result<BackfillSummary, String> {
    let root = match args
        .session_dir
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(dir) => PathBuf::from(dir),
        None => {
            let home = env::var("HOME")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .ok_or_else(|| "HOME environment variable is not set".to_string())?;
            PathBuf::from(home).join(".copilot").join("session-state")
        }
    };

    let days = args.days.unwrap_or(60).max(1) as u64;
    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_secs(days * 86_400))
        .unwrap_or(SystemTime::UNIX_EPOCH);

    let entries = fs::read_dir(&root)
        .map_err(|err| format!("failed to read session dir {}: {err}", root.display()))?;

    let machine = env::var("HOSTNAME")
        .or_else(|_| env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "local-host".to_string());

    let mut summary = BackfillSummary::default();

    let mut session_dirs: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() || is_shadow_bout_session(&path) {
            continue;
        }
        let events_path = path.join("events.jsonl");
        let Ok(meta) = fs::metadata(&events_path) else {
            continue;
        };
        if let Ok(modified) = meta.modified()
            && modified < cutoff
        {
            continue;
        }
        let session_id = entry.file_name().to_string_lossy().to_string();
        session_dirs.push((session_id, events_path));
    }
    session_dirs.sort_by(|a, b| a.0.cmp(&b.0));

    for (session_id, events_path) in session_dirs {
        summary.sessions_scanned += 1;
        let runs = parse_session_events_file(&events_path, &session_id, &machine);
        for mut run in runs {
            if validate_and_normalize_record_run_input(&mut run).is_err() {
                continue;
            }
            summary.runs_found += 1;
            if summary.sample_run_keys.len() < 10 {
                summary.sample_run_keys.push(run.run_key.clone());
            }
            if args.dry_run {
                continue;
            }
            if submit_or_spool_run(client, spool_scope, run).await.is_ok() {
                summary.runs_submitted += 1;
            }
        }
    }

    Ok(summary)
}

#[derive(Deserialize)]
struct SessionEvent {
    #[serde(default, rename = "type")]
    event_type: String,
    #[serde(default)]
    timestamp: String,
    #[serde(default, rename = "agentId")]
    agent_id: String,
    #[serde(default)]
    data: serde_json::Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionStartData {
    #[serde(default)]
    start_time: String,
    #[serde(default)]
    selected_model: String,
    #[serde(default)]
    reasoning_effort: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelChangeData {
    #[serde(default)]
    source: String,
    #[serde(default)]
    new_model: String,
    #[serde(default)]
    reasoning_effort: Option<String>,
}

#[derive(Deserialize)]
struct UserMessageData {
    #[serde(default)]
    source: String,
}

#[derive(Default, Deserialize)]
struct ToolStartArgs {
    #[serde(default)]
    skill: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    agent_type: String,
    #[serde(default)]
    model: String,
    #[serde(default, alias = "reasoningEffort")]
    reasoning_effort: Option<String>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    command: String,
    #[serde(default, rename = "runKey")]
    run_key: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolStartData {
    #[serde(default)]
    tool_call_id: String,
    #[serde(default)]
    tool_name: String,
    #[serde(default)]
    arguments: ToolStartArgs,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolCompleteData {
    #[serde(default)]
    tool_call_id: String,
    #[serde(default)]
    success: Option<bool>,
    #[serde(default)]
    result: serde_json::Value,
}

#[derive(Deserialize)]
struct AssistantMessageData {
    #[serde(default)]
    content: String,
}

#[derive(Clone, Copy)]
struct TaskLegRef {
    on_staged: bool,
    index: usize,
    started_at: Option<DateTime<Utc>>,
}

pub fn parse_session_events_file(
    path: &Path,
    session_id: &str,
    machine: &str,
) -> Vec<RecordRunInput> {
    let Ok(file) = File::open(path) else {
        return Vec::new();
    };
    let reader = BufReader::new(file);

    let mut host_model = String::new();
    let mut host_reasoning_effort = String::new();
    let mut session_start: Option<DateTime<Utc>> = None;
    let mut active_skill = String::new();
    let mut pending_legs: Vec<RunLegInput> = Vec::new();
    let mut known_models: Vec<String> = Vec::new();
    let mut runs: Vec<RecordRunInput> = Vec::new();
    let mut staged_run: Option<RecordRunInput> = None;
    let mut turn_counter: usize = 0;
    let mut live_recorded = false;
    let mut skipped_run = false;
    let mut captured_run_key = String::new();
    let mut pending_record_calls: HashMap<String, bool> = HashMap::new();
    let mut pending_select_calls: HashMap<String, bool> = HashMap::new();
    let mut pending_task_calls: HashMap<String, TaskLegRef> = HashMap::new();

    let has_valid_staged_run = |r: &RecordRunInput| -> bool {
        if r.skill == SKILL_PLAN_TOURNAMENT {
            let succeeded_planners = r
                .legs
                .iter()
                .filter(|l| {
                    TOURNAMENT_LENSES.contains(&l.role.as_str())
                        && (l.status.is_empty() || l.status == "ok" || l.status == "retried")
                })
                .count();
            return succeeded_planners >= 2;
        }
        true
    };

    let flush_staged_run = |staged_run: &mut Option<RecordRunInput>,
                            live_recorded: bool,
                            skipped_run: bool,
                            captured_run_key: &mut String,
                            pending_task_calls: &mut HashMap<String, TaskLegRef>,
                            runs: &mut Vec<RecordRunInput>| {
        if let Some(mut r) = staged_run.take() {
            if !live_recorded && !skipped_run && has_valid_staged_run(&r) {
                if !captured_run_key.is_empty() && r.run_key.starts_with("backfill_") {
                    r.run_key = captured_run_key.clone();
                }
                runs.push(r);
            }
            captured_run_key.clear();
            pending_task_calls.retain(|_, v| !v.on_staged);
        }
    };

    for line_res in reader.lines() {
        let Ok(line) = line_res else {
            continue;
        };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(ev) = serde_json::from_str::<SessionEvent>(&line) else {
            continue;
        };
        if !ev.agent_id.trim().is_empty() {
            continue;
        }

        let ev_time = DateTime::parse_from_rfc3339(&ev.timestamp)
            .ok()
            .map(|dt| dt.with_timezone(&Utc))
            .or(session_start);

        match ev.event_type.as_str() {
            "session.start" => {
                if let Ok(d) = serde_json::from_value::<SessionStartData>(ev.data) {
                    host_model = d.selected_model.trim().to_string();
                    host_reasoning_effort = normalize_effort_lossy(d.reasoning_effort.as_deref());
                    if let Ok(dt) = DateTime::parse_from_rfc3339(&d.start_time) {
                        session_start = Some(dt.with_timezone(&Utc));
                    }
                }
            }
            "session.model_change" => {
                let has_reasoning_effort = ev.data.get("reasoningEffort").is_some();
                if let Ok(d) = serde_json::from_value::<ModelChangeData>(ev.data)
                    && d.source != "agent"
                {
                    if !d.new_model.trim().is_empty() {
                        host_model = d.new_model.trim().to_string();
                        host_reasoning_effort =
                            normalize_effort_lossy(d.reasoning_effort.as_deref());
                    } else if has_reasoning_effort {
                        host_reasoning_effort =
                            normalize_effort_lossy(d.reasoning_effort.as_deref());
                    }
                }
            }
            "user.message" => {
                if let Ok(d) = serde_json::from_value::<UserMessageData>(ev.data)
                    && !d.source.trim().is_empty()
                {
                    continue;
                }
                flush_staged_run(
                    &mut staged_run,
                    live_recorded,
                    skipped_run,
                    &mut captured_run_key,
                    &mut pending_task_calls,
                    &mut runs,
                );
                pending_legs.clear();
                active_skill.clear();
                live_recorded = false;
                skipped_run = false;
                captured_run_key.clear();
                pending_record_calls.clear();
                pending_select_calls.clear();
                pending_task_calls.clear();
            }
            "tool.execution_start" => {
                let Ok(d) = serde_json::from_value::<ToolStartData>(ev.data) else {
                    continue;
                };
                if d.tool_name == "bash" && is_skip_marker_command(&d.arguments.command) {
                    skipped_run = true;
                    staged_run = None;
                    pending_legs.clear();
                    active_skill.clear();
                    captured_run_key.clear();
                    pending_record_calls.clear();
                    pending_select_calls.clear();
                    pending_task_calls.clear();
                    continue;
                }
                let is_select_mcp = d.tool_name.ends_with("select_panel");
                let is_select_bash = d.tool_name == "bash"
                    && (d.arguments.command.contains("model-arena-mcp")
                        || d.arguments.command.contains("model-arena-rust-mcp"))
                    && d.arguments.command.contains("select_panel");
                if is_select_mcp || is_select_bash {
                    flush_staged_run(
                        &mut staged_run,
                        live_recorded,
                        skipped_run,
                        &mut captured_run_key,
                        &mut pending_task_calls,
                        &mut runs,
                    );
                    pending_legs.clear();
                    active_skill.clear();
                    live_recorded = false;
                    skipped_run = false;
                    captured_run_key.clear();
                    pending_record_calls.clear();
                    pending_select_calls.clear();
                    pending_task_calls.clear();
                    if !d.tool_call_id.is_empty() {
                        pending_select_calls.insert(d.tool_call_id, true);
                    }
                    continue;
                }

                let is_record_mcp = d.tool_name.ends_with("record_run");
                let is_record_bash = d.tool_name == "bash"
                    && (d.arguments.command.contains("model-arena-mcp")
                        || d.arguments.command.contains("model-arena-rust-mcp"))
                    && d.arguments.command.contains("record_run");
                if is_record_mcp || is_record_bash {
                    if !d.arguments.run_key.trim().is_empty() {
                        captured_run_key = d.arguments.run_key.trim().to_string();
                    } else if let Some(rk) = extract_run_key_from_text(&d.arguments.command) {
                        captured_run_key = rk;
                    }
                    if !d.tool_call_id.is_empty() {
                        pending_record_calls.insert(d.tool_call_id, is_record_bash);
                    } else {
                        live_recorded = true;
                        staged_run = None;
                        pending_legs.clear();
                        active_skill.clear();
                        captured_run_key.clear();
                    }
                    continue;
                }

                if d.tool_name == "skill"
                    && (d.arguments.skill == SKILL_ADVERSARIAL_REVIEW
                        || d.arguments.skill == SKILL_PLAN_TOURNAMENT)
                {
                    flush_staged_run(
                        &mut staged_run,
                        live_recorded,
                        skipped_run,
                        &mut captured_run_key,
                        &mut pending_task_calls,
                        &mut runs,
                    );
                    pending_legs.clear();
                    live_recorded = false;
                    skipped_run = false;
                    captured_run_key.clear();
                    pending_record_calls.clear();
                    pending_select_calls.clear();
                    pending_task_calls.clear();
                    active_skill = d.arguments.skill;
                }

                if d.tool_name == "task" && !d.arguments.model.trim().is_empty() {
                    let m = d.arguments.model.trim().to_string();
                    let leg_effort =
                        normalize_effort_lossy(d.arguments.reasoning_effort.as_deref());
                    let m_lower = m.to_ascii_lowercase();
                    if !known_models.contains(&m_lower) {
                        known_models.push(m_lower);
                    }
                    if skipped_run {
                        continue;
                    }
                    let combined_task = format!("{} {}", d.arguments.name, d.arguments.description)
                        .to_ascii_lowercase();
                    if let Some(ref mut sr) = staged_run
                        && sr.skill == SKILL_PLAN_TOURNAMENT
                        && (combined_task.contains("critic")
                            || d.arguments.agent_type.eq_ignore_ascii_case("rubber-duck"))
                    {
                        let mut leg_status = "ok".to_string();
                        for prev in &sr.legs {
                            if prev.role == "critic"
                                && (prev.status == "fallback" || prev.status == "timeout")
                            {
                                leg_status = "retried".to_string();
                                break;
                            }
                        }
                        let idx = sr.legs.len();
                        sr.legs.push(RunLegInput {
                            slot_index: idx as i32,
                            role: "critic".to_string(),
                            lineage: String::new(),
                            model: m,
                            reasoning_effort: leg_effort,
                            is_challenger: false,
                            trial_id: None,
                            status: leg_status,
                            duration_ms: 0,
                            won_core_plan: false,
                            cherry_picks_count: 0,
                            critic_adopted_count: 0,
                            critic_bloat_dropped_count: 0,
                        });
                        if !d.tool_call_id.is_empty() {
                            pending_task_calls.insert(
                                d.tool_call_id,
                                TaskLegRef {
                                    on_staged: true,
                                    index: idx,
                                    started_at: ev_time,
                                },
                            );
                        }
                        continue;
                    }

                    let role = classify_task_role(
                        &active_skill,
                        &d.arguments.name,
                        &d.arguments.description,
                        &d.arguments.agent_type,
                        pending_legs.len(),
                    );
                    if !role.is_empty() {
                        if staged_run.is_some() {
                            flush_staged_run(
                                &mut staged_run,
                                live_recorded,
                                skipped_run,
                                &mut captured_run_key,
                                &mut pending_task_calls,
                                &mut runs,
                            );
                            pending_task_calls.retain(|_, v| v.on_staged);
                        }
                        if pending_legs.is_empty() {
                            live_recorded = false;
                            skipped_run = false;
                            pending_record_calls.clear();
                        }
                        let mut leg_status = "ok".to_string();
                        for prev in &pending_legs {
                            if prev.role == role
                                && (prev.status == "fallback" || prev.status == "timeout")
                                && (role != "reviewer"
                                    || infer_lineage(&prev.model, &[]) == infer_lineage(&m, &[]))
                            {
                                leg_status = "retried".to_string();
                                break;
                            }
                        }
                        let idx = pending_legs.len();
                        pending_legs.push(RunLegInput {
                            slot_index: idx as i32,
                            role,
                            lineage: String::new(),
                            model: m,
                            reasoning_effort: leg_effort,
                            is_challenger: false,
                            trial_id: None,
                            status: leg_status,
                            duration_ms: 0,
                            won_core_plan: false,
                            cherry_picks_count: 0,
                            critic_adopted_count: 0,
                            critic_bloat_dropped_count: 0,
                        });
                        if !d.tool_call_id.is_empty() {
                            pending_task_calls.insert(
                                d.tool_call_id,
                                TaskLegRef {
                                    on_staged: false,
                                    index: idx,
                                    started_at: ev_time,
                                },
                            );
                        }
                    }
                }
            }
            "tool.execution_complete" => {
                let Ok(d) = serde_json::from_value::<ToolCompleteData>(ev.data) else {
                    continue;
                };
                if d.tool_call_id.is_empty() {
                    continue;
                }
                let result_text = tool_result_text(&d);
                if pending_select_calls.remove(&d.tool_call_id).is_some()
                    && let Some(rk) = extract_run_key_from_text(&result_text)
                {
                    captured_run_key = rk;
                }
                if let Some(task_ref) = pending_task_calls.remove(&d.tool_call_id) {
                    let duration_ms = match (task_ref.started_at, ev_time) {
                        (Some(start), Some(end)) => {
                            (end - start).num_milliseconds().clamp(0, 86_400_000) as i32
                        }
                        _ => 0,
                    };
                    if task_ref.on_staged {
                        if let Some(ref mut sr) = staged_run
                            && task_ref.index < sr.legs.len()
                        {
                            sr.legs[task_ref.index].duration_ms = duration_ms;
                        }
                    } else if task_ref.index < pending_legs.len() {
                        pending_legs[task_ref.index].duration_ms = duration_ms;
                    }

                    if !tool_complete_succeeded(&d, &result_text, false) {
                        let lower_res = result_text.to_ascii_lowercase();
                        let fail_status =
                            if lower_res.contains("timeout") || lower_res.contains("timed out") {
                                "timeout".to_string()
                            } else {
                                "fallback".to_string()
                            };
                        if task_ref.on_staged {
                            if let Some(ref mut sr) = staged_run
                                && task_ref.index < sr.legs.len()
                            {
                                let failed_role = sr.legs[task_ref.index].role.clone();
                                sr.legs[task_ref.index].status = fail_status;
                                sr.legs[task_ref.index].won_core_plan = false;
                                sr.legs[task_ref.index].cherry_picks_count = 0;
                                if sr.winning_role == failed_role {
                                    let mut reassigned = false;
                                    for l in &mut sr.legs {
                                        if l.role == failed_role
                                            && (l.status.is_empty()
                                                || l.status == "ok"
                                                || l.status == "retried")
                                        {
                                            l.won_core_plan = true;
                                            l.cherry_picks_count = 0;
                                            reassigned = true;
                                            break;
                                        }
                                    }
                                    if !reassigned {
                                        sr.winning_role.clear();
                                    }
                                }
                            }
                        } else if task_ref.index < pending_legs.len() {
                            pending_legs[task_ref.index].status = fail_status;
                            pending_legs[task_ref.index].won_core_plan = false;
                            pending_legs[task_ref.index].cherry_picks_count = 0;
                        }
                    }
                }
                if let Some(is_bash) = pending_record_calls.remove(&d.tool_call_id)
                    && tool_complete_succeeded(&d, &result_text, is_bash)
                {
                    live_recorded = true;
                    staged_run = None;
                    pending_legs.clear();
                    active_skill.clear();
                    captured_run_key.clear();
                }
            }
            "assistant.message" => {
                turn_counter += 1;
                if live_recorded || skipped_run {
                    continue;
                }
                let Ok(d) = serde_json::from_value::<AssistantMessageData>(ev.data) else {
                    continue;
                };
                if d.content.trim().is_empty() {
                    continue;
                }
                let has_new_legs = !pending_legs.is_empty();
                let candidate_legs = if has_new_legs {
                    pending_legs.clone()
                } else if let Some(ref sr) = staged_run
                    && sr.skill == SKILL_ADVERSARIAL_REVIEW
                {
                    sr.legs.clone()
                } else {
                    Vec::new()
                };

                if let Some(mut parsed) = extract_adversarial_review_report(
                    &d.content,
                    session_id,
                    turn_counter,
                    &host_model,
                    &host_reasoning_effort,
                    machine,
                    ev_time,
                    &candidate_legs,
                    &known_models,
                ) {
                    if has_new_legs
                        || staged_run
                            .as_ref()
                            .is_none_or(|sr| sr.skill != SKILL_ADVERSARIAL_REVIEW)
                    {
                        flush_staged_run(
                            &mut staged_run,
                            live_recorded,
                            skipped_run,
                            &mut captured_run_key,
                            &mut pending_task_calls,
                            &mut runs,
                        );
                    } else if let Some(ref sr) = staged_run {
                        parsed.run_key = sr.run_key.clone();
                        parsed.legs = sr.legs.clone();
                    }
                    staged_run = Some(parsed);
                    if has_new_legs {
                        for v in pending_task_calls.values_mut() {
                            v.on_staged = true;
                        }
                    }
                    pending_legs.clear();
                    active_skill.clear();
                    continue;
                }

                if pending_legs.is_empty()
                    && let Some(ref mut sr) = staged_run
                    && sr.skill == SKILL_PLAN_TOURNAMENT
                {
                    if let Some(parsed) = extract_plan_tournament_report(
                        &d.content,
                        session_id,
                        turn_counter,
                        &host_model,
                        &host_reasoning_effort,
                        machine,
                        ev_time,
                        &sr.legs,
                    ) {
                        if sr.winning_role.is_empty() && !parsed.winning_role.is_empty() {
                            sr.winning_role = parsed.winning_role;
                            for (i, l) in sr.legs.iter_mut().enumerate() {
                                if parsed.legs.get(i).is_some_and(|p| p.won_core_plan) {
                                    l.won_core_plan = true;
                                    l.cherry_picks_count = 0;
                                }
                            }
                        }
                        for (i, l) in sr.legs.iter_mut().enumerate() {
                            if let Some(p) = parsed.legs.get(i)
                                && !l.won_core_plan
                                && p.cherry_picks_count > l.cherry_picks_count
                            {
                                l.cherry_picks_count = p.cherry_picks_count;
                            }
                        }
                        if sr.artifact_ref.is_empty() && !parsed.artifact_ref.is_empty() {
                            sr.artifact_ref = parsed.artifact_ref;
                            sr.repo = parsed.repo;
                        }
                    }
                    continue;
                }

                if let Some(parsed) = extract_plan_tournament_report(
                    &d.content,
                    session_id,
                    turn_counter,
                    &host_model,
                    &host_reasoning_effort,
                    machine,
                    ev_time,
                    &pending_legs,
                ) {
                    flush_staged_run(
                        &mut staged_run,
                        live_recorded,
                        skipped_run,
                        &mut captured_run_key,
                        &mut pending_task_calls,
                        &mut runs,
                    );
                    staged_run = Some(parsed);
                    for v in pending_task_calls.values_mut() {
                        v.on_staged = true;
                    }
                    pending_legs.clear();
                    active_skill.clear();
                }
            }
            _ => {}
        }
    }

    flush_staged_run(
        &mut staged_run,
        live_recorded,
        skipped_run,
        &mut captured_run_key,
        &mut pending_task_calls,
        &mut runs,
    );
    runs
}

fn tool_result_text(d: &ToolCompleteData) -> String {
    match &d.result {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Object(map) => map
            .get("content")
            .and_then(|v| v.as_str())
            .map(ToString::to_string)
            .unwrap_or_else(|| d.result.to_string()),
        other => other.to_string(),
    }
}

fn tool_complete_succeeded(d: &ToolCompleteData, result_text: &str, is_bash: bool) -> bool {
    if d.success == Some(false) {
        return false;
    }
    if !is_bash || result_text.is_empty() {
        return true;
    }
    let lower = result_text.to_ascii_lowercase();
    if lower.contains("completed with exit code") && !lower.contains("completed with exit code 0") {
        return false;
    }
    true
}

fn is_skip_marker_command(cmd: &str) -> bool {
    cmd.contains("model-arena:skip") || cmd.contains("model-arena:delegated")
}

fn extract_run_key_from_text(s: &str) -> Option<String> {
    if let Some(idx) = s.find("\"runKey\"") {
        let rest = &s[idx + "\"runKey\"".len()..];
        if let Some(colon) = rest.find(':') {
            let after_colon = rest[colon + 1..].trim_start();
            if let Some(stripped) = after_colon.strip_prefix('"')
                && let Some(end_q) = stripped.find('"')
            {
                let val = stripped[..end_q].trim();
                if !val.is_empty() {
                    return Some(val.to_string());
                }
            }
        }
    }
    for marker in ["--arg runKey \"", "--arg runKey '"] {
        if let Some(idx) = s.find(marker) {
            let quote = marker.chars().last().unwrap();
            let rest = &s[idx + marker.len()..];
            if let Some(end_q) = rest.find(quote) {
                let val = rest[..end_q].trim();
                if !val.is_empty() && !val.starts_with('$') {
                    return Some(val.to_string());
                }
            }
        }
    }
    None
}

fn classify_task_role(
    active_skill: &str,
    name: &str,
    desc: &str,
    agent_type: &str,
    idx: usize,
) -> String {
    let combined = format!("{name} {desc}").to_ascii_lowercase();
    if active_skill == SKILL_ADVERSARIAL_REVIEW {
        if combined.contains("ponytail") {
            return "ponytail".to_string();
        }
        return "reviewer".to_string();
    }
    if active_skill == SKILL_PLAN_TOURNAMENT
        || combined.contains("planner")
        || combined.contains("minimal diff")
        || combined.contains("clean architecture")
        || combined.contains("edge case")
    {
        if combined.contains("scout") {
            return "explore_scout".to_string();
        }
        if combined.contains("critic") || agent_type == "rubber-duck" {
            return "critic".to_string();
        }
        if combined.contains("minimal") {
            return "minimal_diff".to_string();
        }
        if combined.contains("clean") || combined.contains("arch") {
            return "clean_architecture".to_string();
        }
        if combined.contains("edge") {
            return "edge_cases".to_string();
        }
        return TOURNAMENT_LENSES[idx % TOURNAMENT_LENSES.len()].to_string();
    }
    if combined.contains("adversarial") || combined.contains("review") {
        if combined.contains("ponytail") {
            return "ponytail".to_string();
        }
        return "reviewer".to_string();
    }
    String::new()
}

fn is_model_char(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_' || b == b'.'
}

fn contains_whole_model_token(haystack_lower: &str, model_lower: &str) -> bool {
    if model_lower.is_empty() {
        return false;
    }
    let hay_bytes = haystack_lower.as_bytes();
    for (pos, _) in haystack_lower.match_indices(model_lower) {
        let end = pos + model_lower.len();
        let left_ok = pos == 0 || !is_model_char(hay_bytes[pos - 1]);
        let mut right_ok = end == hay_bytes.len() || !is_model_char(hay_bytes[end]);
        if !right_ok
            && hay_bytes[end] == b'.'
            && (end + 1 == hay_bytes.len() || !is_model_char(hay_bytes[end + 1]))
        {
            right_ok = true;
        }
        if left_ok && right_ok {
            return true;
        }
    }
    false
}

fn extract_model_ids(text_lower: &str) -> Vec<String> {
    const PREFIXES: [&str; 8] = [
        "gpt-", "claude-", "gemini-", "grok-", "mai-", "o1-", "o3-", "o4-",
    ];
    let mut out = Vec::new();
    let bytes = text_lower.as_bytes();
    for (i, _) in text_lower.char_indices() {
        if i > 0 && is_model_char(bytes[i - 1]) {
            continue;
        }
        let rest = &text_lower[i..];
        if PREFIXES.iter().any(|p| rest.starts_with(p)) {
            let len = rest
                .bytes()
                .take_while(|&b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.'
                })
                .count();
            let token = rest[..len].trim_end_matches(['.', '-']);
            if token.contains('-') && token.len() >= 4 {
                let s = token.to_string();
                if !out.contains(&s) {
                    out.push(s);
                }
            }
        }
    }
    out
}

fn extract_repo_ref(content: &str) -> Option<(String, String)> {
    for token in content.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '(' | ')' | '[' | ']' | '<' | '>' | '"' | '\'' | '`' | ',' | ';'
            )
    }) {
        let trimmed = token.trim_end_matches(['.', ':', ';']);
        let Some(hash_idx) = trimmed.find('#') else {
            continue;
        };
        let repo_part = &trimmed[..hash_idx];
        let num_part = &trimmed[hash_idx + 1..];
        if num_part.is_empty() || !num_part.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Some(slash_idx) = repo_part.find('/') else {
            continue;
        };
        let owner = &repo_part[..slash_idx];
        let name = &repo_part[slash_idx + 1..];
        let valid_ident = |s: &str| {
            !s.is_empty()
                && !s.contains('/')
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
        };
        if valid_ident(owner) && valid_ident(name) {
            return Some((trimmed.to_string(), repo_part.to_string()));
        }
    }
    None
}

fn extract_approach_verdict(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line
            .trim()
            .trim_start_matches('-')
            .trim()
            .trim_start_matches('*');
        let lower = trimmed.to_ascii_lowercase();
        let Some(after) = lower.strip_prefix("approach") else {
            continue;
        };
        let rest = after
            .trim_start_matches('*')
            .trim_start_matches(':')
            .trim_start_matches('*')
            .trim()
            .trim_start_matches('`');
        for candidate in [
            "insufficient_evidence",
            "insufficient evidence",
            "insufficient-evidence",
            "rethink",
            "adjust",
            "keep",
        ] {
            if let Some(tail) = rest.strip_prefix(candidate)
                && (tail.is_empty()
                    || tail.starts_with(|c: char| {
                        c.is_whitespace() || matches!(c, '`' | '*' | '-' | '(' | '.' | ',' | ';')
                    }))
            {
                return Some(candidate.replace([' ', '-'], "_"));
            }
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn extract_adversarial_review_report(
    content: &str,
    session_id: &str,
    turn_index: usize,
    host_model: &str,
    host_reasoning_effort: &str,
    machine: &str,
    occurred_at: Option<DateTime<Utc>>,
    pending_legs: &[RunLegInput],
    known_models: &[String],
) -> Option<RecordRunInput> {
    let lower = content.to_ascii_lowercase();
    if !lower.contains("set aside")
        || (!lower.contains("adversarial") && !lower.contains("approach:"))
    {
        return None;
    }
    let verdict = extract_approach_verdict(content)?;

    let mut legs: Vec<RunLegInput> = pending_legs
        .iter()
        .filter(|l| l.role == "reviewer" || l.role == "ponytail")
        .cloned()
        .collect();
    for (idx, leg) in legs.iter_mut().enumerate() {
        leg.slot_index = idx as i32;
    }
    let mut all_known = known_models.to_vec();
    if !legs.iter().any(|l| l.role == "reviewer") {
        legs.clear();
        for line in content.lines() {
            let line_lower = line.to_ascii_lowercase();
            if line_lower.contains("adversarial") {
                for m in extract_model_ids(&line_lower) {
                    if !legs.iter().any(|l| l.model.eq_ignore_ascii_case(&m)) {
                        let slot_index = legs.len() as i32;
                        legs.push(RunLegInput {
                            slot_index,
                            role: "reviewer".to_string(),
                            lineage: String::new(),
                            model: m.clone(),
                            reasoning_effort: String::new(),
                            is_challenger: false,
                            trial_id: None,
                            status: "ok".to_string(),
                            duration_ms: 0,
                            won_core_plan: false,
                            cherry_picks_count: 0,
                            critic_adopted_count: 0,
                            critic_bloat_dropped_count: 0,
                        });
                    }
                    if !all_known.contains(&m) {
                        all_known.push(m);
                    }
                }
                if !legs.is_empty() {
                    break;
                }
            }
        }
    }
    if !legs.iter().any(|l| l.role == "reviewer") {
        return None;
    }

    let candidates = parse_report_candidates(content, &legs);
    let (artifact_ref, repo, artifact_kind) = match extract_repo_ref(content) {
        Some((r_ref, r_repo)) => (r_ref, r_repo, "pr_diff".to_string()),
        None => (String::new(), String::new(), "branch_diff".to_string()),
    };

    let mut hasher = Sha256::new();
    hasher.update(format!("{session_id}:{turn_index}:{artifact_ref}").as_bytes());
    let digest = hasher.finalize();
    let run_key = format!("backfill_{}", hex::encode(&digest[..12]));
    let event_ts = occurred_at.unwrap_or_else(Utc::now);

    Some(RecordRunInput {
        run_key,
        skill: SKILL_ADVERSARIAL_REVIEW.to_string(),
        artifact_kind,
        artifact_ref,
        repo,
        phase: "independent".to_string(),
        host_model: host_model.to_string(),
        host_reasoning_effort: host_reasoning_effort.to_string(),
        host_lineage: String::new(),
        harness: "copilot-backfill".to_string(),
        machine: machine.to_string(),
        approach_verdict: verdict,
        winning_role: String::new(),
        occurred_at: Some(event_ts),
        client_sent_at: Some(event_ts),
        queue_only: false,
        legs,
        candidates,
    })
}

fn is_winning_clause(clause_lower: &str) -> bool {
    const NEGATED: [&str; 12] = [
        "non-winning",
        "non winning",
        "not-winning",
        "not winning",
        "didn't win",
        "did not win",
        "wasn't selected",
        "was not selected",
        "not selected",
        "not adopted",
        "wasn't adopted",
        "was not adopted",
    ];
    if NEGATED.iter().any(|n| clause_lower.contains(n))
        || contains_whole_model_token(clause_lower, "rejected")
        || contains_whole_model_token(clause_lower, "lost")
    {
        return false;
    }
    for pos in ["won", "winner", "winning", "selected", "adopted"] {
        if contains_whole_model_token(clause_lower, pos) {
            return true;
        }
    }
    clause_lower.contains("core plan")
}

fn role_cues_for(role: &str) -> &'static [&'static str] {
    match role {
        "minimal_diff" => &["planner a", "minimal diff", "minimal_diff"],
        "clean_architecture" => &[
            "planner b",
            "cleanest architecture",
            "clean architecture",
            "clean_architecture",
        ],
        "edge_cases" => &["planner c", "edge-case", "edge case", "edge_cases"],
        _ => &[],
    }
}

#[allow(clippy::too_many_arguments)]
fn extract_plan_tournament_report(
    content: &str,
    session_id: &str,
    turn_index: usize,
    host_model: &str,
    host_reasoning_effort: &str,
    machine: &str,
    occurred_at: Option<DateTime<Utc>>,
    pending_legs: &[RunLegInput],
) -> Option<RecordRunInput> {
    let is_succeeded =
        |l: &RunLegInput| l.status.is_empty() || l.status == "ok" || l.status == "retried";
    let planner_legs = pending_legs
        .iter()
        .filter(|l| TOURNAMENT_LENSES.contains(&l.role.as_str()) && is_succeeded(l))
        .count();
    if planner_legs < 2 {
        return None;
    }

    let lower = content.to_ascii_lowercase();
    if !lower.contains("hybrid plan")
        && !lower.contains("winning")
        && !lower.contains("planner a")
        && !lower.contains("minimal diff")
    {
        return None;
    }

    let mut legs: Vec<RunLegInput> = pending_legs
        .iter()
        .filter(|l| {
            matches!(
                l.role.as_str(),
                "explore_scout" | "minimal_diff" | "clean_architecture" | "edge_cases" | "critic"
            )
        })
        .cloned()
        .collect();
    for (idx, l) in legs.iter_mut().enumerate() {
        l.slot_index = idx as i32;
        l.won_core_plan = false;
        l.cherry_picks_count = 0;
    }
    let mut winning_role = String::new();
    let lines: Vec<&str> = lower.lines().collect();

    // Pass 1: explicit role/label cues
    for raw_line in &lines {
        let win_clause = match raw_line.find("cherry-pick") {
            Some(cp_idx) => &raw_line[..cp_idx],
            None => *raw_line,
        };
        if !is_winning_clause(win_clause) {
            continue;
        }
        for l in &legs {
            if !is_succeeded(l) {
                continue;
            }
            if role_cues_for(&l.role)
                .iter()
                .any(|cue| win_clause.contains(cue))
            {
                winning_role = l.role.clone();
                break;
            }
        }
        if !winning_role.is_empty() {
            break;
        }
    }

    // Pass 2: whole-token model match on winning clause
    if winning_role.is_empty() {
        for raw_line in &lines {
            let win_clause = match raw_line.find("cherry-pick") {
                Some(cp_idx) => &raw_line[..cp_idx],
                None => *raw_line,
            };
            if !is_winning_clause(win_clause) {
                continue;
            }
            for l in &legs {
                if is_succeeded(l)
                    && TOURNAMENT_LENSES.contains(&l.role.as_str())
                    && contains_whole_model_token(win_clause, &l.model.to_ascii_lowercase())
                {
                    winning_role = l.role.clone();
                    break;
                }
            }
            if !winning_role.is_empty() {
                break;
            }
        }
    }

    // Extract cherry-pick clauses
    let mut cherry_clauses: Vec<String> = Vec::new();
    let mut in_cherry_section = false;
    for raw_line in &lines {
        let trimmed = raw_line.trim();
        let is_bullet = trimmed.starts_with("- ")
            || trimmed.starts_with("* ")
            || trimmed == "-"
            || trimmed == "*";
        let raw_trimmed = trimmed.trim_start_matches('#').trim();
        let is_bold_heading = !is_bullet
            && raw_trimmed.starts_with("**")
            && (raw_trimmed.ends_with("**")
                || raw_trimmed.ends_with("**:")
                || raw_trimmed.ends_with(":**"));
        let is_heading = trimmed.starts_with('#') || is_bold_heading;
        if is_heading {
            in_cherry_section = raw_trimmed.contains("cherry-pick")
                && !raw_trimmed.contains("none")
                && !raw_trimmed.contains("no cherry-pick");
        }
        if let Some(idx) = trimmed.find("cherry-pick") {
            let prefix = &trimmed[..idx];
            let negated_before = prefix.ends_with("no ") || prefix.ends_with("without ");
            let clause = &trimmed[idx..];
            if !negated_before
                && !clause.contains("cherry-picks: none")
                && !clause.contains("cherry-pick: none")
                && !clause.contains("no cherry-pick")
            {
                cherry_clauses.push(clause.to_string());
            }
            continue;
        }
        if in_cherry_section
            && (trimmed.starts_with('-') || trimmed.starts_with('*'))
            && !trimmed.contains("none")
            && !trimmed.contains("no cherry-pick")
        {
            cherry_clauses.push(trimmed.to_string());
        }
    }

    if winning_role.is_empty() && cherry_clauses.is_empty() && !lower.contains("hybrid plan") {
        return None;
    }

    let mentioned_in_cherry_pick = |l: &RunLegInput| -> bool {
        let model_lower = l.model.to_ascii_lowercase();
        let cues = role_cues_for(&l.role);
        for clause in &cherry_clauses {
            if contains_whole_model_token(clause, &model_lower) {
                return true;
            }
            if cues.iter().any(|cue| clause.contains(cue)) {
                return true;
            }
        }
        false
    };

    let mut won_assigned = false;
    for l in &mut legs {
        if !is_succeeded(l) {
            continue;
        }
        if !winning_role.is_empty() && l.role == winning_role && !won_assigned {
            l.won_core_plan = true;
            won_assigned = true;
        } else if TOURNAMENT_LENSES.contains(&l.role.as_str()) && mentioned_in_cherry_pick(l) {
            l.cherry_picks_count = 1;
        }
    }

    let (artifact_ref, repo) = extract_repo_ref(content).unwrap_or_default();

    let mut hasher = Sha256::new();
    hasher.update(format!("pt:{session_id}:{turn_index}:{artifact_ref}").as_bytes());
    let digest = hasher.finalize();
    let run_key = format!("backfill_{}", hex::encode(&digest[..12]));
    let event_ts = occurred_at.unwrap_or_else(Utc::now);

    Some(RecordRunInput {
        run_key,
        skill: SKILL_PLAN_TOURNAMENT.to_string(),
        artifact_kind: "plan".to_string(),
        artifact_ref,
        repo,
        phase: "independent".to_string(),
        host_model: host_model.to_string(),
        host_reasoning_effort: host_reasoning_effort.to_string(),
        host_lineage: String::new(),
        harness: "copilot-backfill".to_string(),
        machine: machine.to_string(),
        approach_verdict: String::new(),
        winning_role,
        occurred_at: Some(event_ts),
        client_sent_at: Some(event_ts),
        queue_only: false,
        legs,
        candidates: Vec::new(),
    })
}

fn parse_report_candidates(content: &str, legs: &[RunLegInput]) -> Vec<CandidateInput> {
    let mut reviewer_models: Vec<String> = Vec::new();
    let mut has_ponytail_leg = false;
    for l in legs {
        if l.role == "ponytail" {
            has_ponytail_leg = true;
            continue;
        }
        if l.role == "reviewer" {
            let m = l.model.to_ascii_lowercase();
            if !reviewer_models.contains(&m) {
                reviewer_models.push(m);
            }
        }
    }

    let mut section = "";
    let mut out = Vec::new();

    let mut append_bullet = |sec: &str, raw_bullet: &str| {
        let bullet = raw_bullet.trim();
        let lower_bullet = bullet.to_ascii_lowercase();
        if bullet.is_empty()
            || lower_bullet == "none"
            || lower_bullet.starts_with("none ")
            || lower_bullet.starts_with("none.")
        {
            return;
        }
        let mut proposers: Vec<String> = reviewer_models
            .iter()
            .filter(|m| contains_whole_model_token(&lower_bullet, m))
            .cloned()
            .collect();
        if has_ponytail_leg
            && (lower_bullet.contains("[ponytail]")
                || contains_whole_model_token(&lower_bullet, "ponytail"))
            && !proposers.iter().any(|p| p == "ponytail")
        {
            proposers.push("ponytail".to_string());
        }
        if proposers.is_empty() {
            return;
        }
        let summary = if bullet.chars().count() > 180 {
            let truncated: String = bullet.chars().take(177).collect();
            format!("{truncated}...")
        } else {
            bullet.to_string()
        };
        out.push(CandidateInput {
            candidate_key: String::new(),
            summary,
            severity: "important".to_string(),
            disposition: sec.to_string(),
            caused_approach_pivot: false,
            proposing_models: proposers,
        });
    };

    let inline_remainder = |line: &str| -> String {
        match line.find(':') {
            Some(idx) => line[idx + 1..]
                .trim()
                .trim_start_matches(['*', '`', ' '])
                .to_string(),
            None => String::new(),
        }
    };

    for raw_line in content.lines() {
        let trimmed = raw_line.trim();
        let clean_header = trimmed
            .trim_matches(|c: char| matches!(c, '#' | '*' | '-' | ':' | '`' | ' '))
            .to_ascii_lowercase();
        if clean_header.starts_with("findings") || clean_header.starts_with("adopted") {
            section = "adopted";
            let rem = inline_remainder(trimmed);
            append_bullet(section, &rem);
            continue;
        }
        if clean_header.starts_with("set aside") {
            section = "set_aside";
            let rem = inline_remainder(trimmed);
            append_bullet(section, &rem);
            continue;
        }
        if clean_header.starts_with("open questions") {
            section = "open_question";
            let rem = inline_remainder(trimmed);
            append_bullet(section, &rem);
            continue;
        }
        if clean_header.starts_with("coverage")
            || clean_header.starts_with("status")
            || trimmed.starts_with("```")
        {
            section = "";
            continue;
        }
        if section.is_empty() || (!trimmed.starts_with("- ") && !trimmed.starts_with("* ")) {
            continue;
        }
        append_bullet(section, &trimmed[2..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parses_adversarial_review_and_subagent_durations_from_events_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let events_path = dir.path().join("events.jsonl");
        let mut f = File::create(&events_path).unwrap();

        let lines = [
            r#"{"type":"session.start","timestamp":"2026-03-01T10:00:00Z","data":{"startTime":"2026-03-01T10:00:00Z","selectedModel":"gemini-4-argon"}}"#,
            r#"{"type":"user.message","timestamp":"2026-03-01T10:00:05Z","data":{"content":"Review PR owner/repo#42"}}"#,
            r#"{"type":"tool.execution_start","timestamp":"2026-03-01T10:00:06Z","data":{"toolCallId":"sel_1","toolName":"model-arena-select_panel","arguments":{"skill":"adversarial-review"}}}"#,
            r#"{"type":"tool.execution_complete","timestamp":"2026-03-01T10:00:07Z","data":{"toolCallId":"sel_1","success":true,"result":"{\"runKey\":\"run_custom_key_42\"}"}}"#,
            r#"{"type":"tool.execution_start","timestamp":"2026-03-01T10:00:10Z","data":{"toolCallId":"t_1","toolName":"task","arguments":{"name":"rev-a","description":"Adversarial review leg 1","agent_type":"general-purpose","model":"gpt-6.1-sol"}}}"#,
            r#"{"type":"tool.execution_start","timestamp":"2026-03-01T10:00:11Z","data":{"toolCallId":"t_2","toolName":"task","arguments":{"name":"rev-b","description":"Adversarial review leg 2","agent_type":"general-purpose","model":"claude-opus-5.5"}}}"#,
            r#"{"type":"tool.execution_complete","timestamp":"2026-03-01T10:00:55Z","data":{"toolCallId":"t_1","success":true,"result":"finding 1"}}"#,
            r#"{"type":"tool.execution_complete","timestamp":"2026-03-01T10:01:31Z","data":{"toolCallId":"t_2","success":true,"result":"finding 2"}}"#,
            r####"{"type":"assistant.message","timestamp":"2026-03-01T10:02:00Z","data":{"content":"### Adversarial review (gpt-6.1-sol, claude-opus-5.5) on owner/repo#42\n- **Approach:** `keep`\n- **Findings:**\n  - Missing transaction rollback on error (gpt-6.1-sol, claude-opus-5.5)\n- **Set aside:**\n  - Nit on variable naming (claude-opus-5.5)\n- **Open questions:** none"}}"####,
        ];
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }

        let runs = parse_session_events_file(&events_path, "sess_1", "macbook");
        assert_eq!(runs.len(), 1);
        let r = &runs[0];
        assert_eq!(r.run_key, "run_custom_key_42");
        assert_eq!(r.skill, SKILL_ADVERSARIAL_REVIEW);
        assert_eq!(r.artifact_kind, "pr_diff");
        assert_eq!(r.artifact_ref, "owner/repo#42");
        assert_eq!(r.repo, "owner/repo");
        assert_eq!(r.approach_verdict, "keep");
        assert_eq!(r.legs.len(), 2);
        assert_eq!(r.legs[0].model, "gpt-6.1-sol");
        assert_eq!(r.legs[0].duration_ms, 45_000);
        assert_eq!(r.legs[1].model, "claude-opus-5.5");
        assert_eq!(r.legs[1].duration_ms, 80_000);
        assert_eq!(r.candidates.len(), 2);
        assert_eq!(r.candidates[0].disposition, "adopted");
        assert_eq!(
            r.candidates[0].proposing_models,
            vec!["gpt-6.1-sol", "claude-opus-5.5"]
        );
        assert_eq!(r.candidates[1].disposition, "set_aside");
        assert_eq!(r.candidates[1].proposing_models, vec!["claude-opus-5.5"]);
        assert_eq!(r.client_sent_at, r.occurred_at);
    }

    #[test]
    fn handles_unicode_tokens_and_edge_case_descriptions_under_adversarial_review() {
        assert!(!contains_whole_model_token(
            "compared against xé-model in bullet",
            "é-model"
        ));
        assert!(contains_whole_model_token(
            "found by é-model in bullet",
            "é-model"
        ));
        assert_eq!(
            classify_task_role(
                SKILL_ADVERSARIAL_REVIEW,
                "rev-edge",
                "Check edge case handling in parser",
                "rubber-duck",
                0
            ),
            "reviewer"
        );
    }

    #[test]
    fn parses_nullable_reasoning_effort_and_resets_host_effort_on_model_change() {
        let dir = tempfile::tempdir().unwrap();
        let events_path = dir.path().join("events.jsonl");
        let mut f = File::create(&events_path).unwrap();

        let lines = [
            r#"{"type":"session.start","timestamp":"2026-03-02T10:00:00Z","data":{"startTime":"2026-03-02T10:00:00Z","selectedModel":"claude-opus-5.5","reasoningEffort":"high"}}"#,
            r#"{"type":"session.model_change","timestamp":"2026-03-02T10:00:01Z","data":{"source":"user","reasoningEffort":"xhigh"}}"#,
            r#"{"type":"session.model_change","timestamp":"2026-03-02T10:00:02Z","data":{"source":"user"}}"#,
            r#"{"type":"session.model_change","timestamp":"2026-03-02T10:00:03Z","data":{"source":"user","newModel":"gemini-4-neon"}}"#,
            r#"{"type":"skill.invoked","timestamp":"2026-03-02T10:00:05Z","data":{"name":"adversarial-review"}}"#,
            r#"{"type":"tool.execution_start","timestamp":"2026-03-02T10:00:10Z","data":{"toolCallId":"t1","toolName":"task","arguments":{"name":"rev-a","agent_type":"rubber-duck","model":"gpt-6.2-sol","reasoning_effort":"xhigh","description":"Adversarial review"}}}"#,
            r#"{"type":"tool.execution_complete","timestamp":"2026-03-02T10:00:40Z","data":{"toolCallId":"t1","success":true}}"#,
            r#"{"type":"tool.execution_start","timestamp":"2026-03-02T10:00:10Z","data":{"toolCallId":"t2","toolName":"task","arguments":{"name":"rev-b","agent_type":"rubber-duck","model":"claude-opus-5.5","reasoningEffort":"turbo","description":"Adversarial review"}}}"#,
            r#"{"type":"tool.execution_complete","timestamp":"2026-03-02T10:00:50Z","data":{"toolCallId":"t2","success":true}}"#,
            r#"{"type":"assistant.message","timestamp":"2026-03-02T10:01:00Z","data":{"content":"Adversarial review for owner/repo#99\nApproach: keep\nFindings:\n- gpt-6.2-sol caught race condition\nSet aside:\n- claude-opus-5.5 style nit"}}"#,
            r#"{"type":"user.message","timestamp":"2026-03-02T10:02:00Z","data":{"content":"Second review"}}"#,
            r#"{"type":"session.model_change","timestamp":"2026-03-02T10:02:01Z","data":{"source":"user","reasoningEffort":"medium"}}"#,
            r#"{"type":"session.model_change","timestamp":"2026-03-02T10:02:02Z","data":{"source":"user"}}"#,
            r#"{"type":"tool.execution_start","timestamp":"2026-03-02T10:02:05Z","data":{"toolCallId":"t3","toolName":"task","arguments":{"name":"rev-c","agent_type":"rubber-duck","model":"gpt-6.2-sol","reasoning_effort":"high","description":"Adversarial review"}}}"#,
            r#"{"type":"tool.execution_start","timestamp":"2026-03-02T10:02:06Z","data":{"toolCallId":"t4","toolName":"task","arguments":{"name":"rev-d","agent_type":"rubber-duck","model":"claude-opus-5.5","reasoning_effort":"high","description":"Adversarial review"}}}"#,
            r#"{"type":"tool.execution_complete","timestamp":"2026-03-02T10:02:25Z","data":{"toolCallId":"t3","success":true}}"#,
            r#"{"type":"tool.execution_complete","timestamp":"2026-03-02T10:02:26Z","data":{"toolCallId":"t4","success":true}}"#,
            r#"{"type":"assistant.message","timestamp":"2026-03-02T10:02:30Z","data":{"content":"Adversarial review for owner/repo#100\nApproach: keep\nFindings: none\nSet aside: none"}}"#,
            r#"{"type":"user.message","timestamp":"2026-03-02T10:03:00Z","data":{"content":"Third review"}}"#,
            r#"{"type":"session.model_change","timestamp":"2026-03-02T10:03:01Z","data":{"source":"user","reasoningEffort":null}}"#,
            r#"{"type":"tool.execution_start","timestamp":"2026-03-02T10:03:05Z","data":{"toolCallId":"t5","toolName":"task","arguments":{"name":"rev-e","agent_type":"rubber-duck","model":"gpt-6.2-sol","reasoning_effort":"high","description":"Adversarial review"}}}"#,
            r#"{"type":"tool.execution_start","timestamp":"2026-03-02T10:03:06Z","data":{"toolCallId":"t6","toolName":"task","arguments":{"name":"rev-f","agent_type":"rubber-duck","model":"claude-opus-5.5","reasoning_effort":"high","description":"Adversarial review"}}}"#,
            r#"{"type":"tool.execution_complete","timestamp":"2026-03-02T10:03:25Z","data":{"toolCallId":"t5","success":true}}"#,
            r#"{"type":"tool.execution_complete","timestamp":"2026-03-02T10:03:26Z","data":{"toolCallId":"t6","success":true}}"#,
            r#"{"type":"assistant.message","timestamp":"2026-03-02T10:03:30Z","data":{"content":"Adversarial review for owner/repo#101\nApproach: keep\nFindings: none\nSet aside: none"}}"#,
        ];
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }
        drop(f);

        let runs = parse_session_events_file(&events_path, "sess-effort", "macbook");
        assert_eq!(runs.len(), 3);
        let r = &runs[0];
        assert_eq!(r.host_model, "gemini-4-neon");
        assert_eq!(r.host_reasoning_effort, "");
        assert_eq!(r.legs.len(), 2);
        assert_eq!(r.legs[0].model, "gpt-6.2-sol");
        assert_eq!(r.legs[0].reasoning_effort, "xhigh");
        assert_eq!(r.legs[1].model, "claude-opus-5.5");
        assert_eq!(r.legs[1].reasoning_effort, "");

        let r2 = &runs[1];
        assert_eq!(r2.host_model, "gemini-4-neon");
        assert_eq!(r2.host_reasoning_effort, "medium");

        let r3 = &runs[2];
        assert_eq!(r3.host_model, "gemini-4-neon");
        assert_eq!(r3.host_reasoning_effort, "");
    }

    #[test]
    fn detects_and_skips_shadow_bout_sessions_from_workspace_yaml() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_shadow_bout_session(dir.path()));

        fs::write(
            dir.path().join("workspace.yaml"),
            "id: abc\nname: regular-session\n",
        )
        .unwrap();
        assert!(!is_shadow_bout_session(dir.path()));

        fs::write(
            dir.path().join("workspace.yaml"),
            "id: abc\nname: \"shadow-bout:sb_123:challenger\"\n",
        )
        .unwrap();
        assert!(is_shadow_bout_session(dir.path()));
    }
}
