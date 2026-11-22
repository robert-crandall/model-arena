-- Model Arena tables: per-skill lineage rosters, challenger trials,
-- lens rotation state, recorded skill runs, per-leg outcomes (with timing),
-- attributed findings/candidates, and single-model coding PR profiles.

CREATE TABLE arena_roster (
    id BIGSERIAL PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    skill TEXT NOT NULL CHECK (skill IN ('adversarial-review', 'plan-tournament')),
    lineage TEXT NOT NULL,
    champion_model TEXT NOT NULL,
    backup_model TEXT NOT NULL DEFAULT '',
    sort_order INT NOT NULL DEFAULT 0,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (user_id, skill, lineage)
);

CREATE INDEX idx_arena_roster_user_skill ON arena_roster (user_id, skill, sort_order);

CREATE TABLE arena_trials (
    id BIGSERIAL PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    skill TEXT NOT NULL CHECK (skill IN ('adversarial-review', 'plan-tournament')),
    lineage TEXT NOT NULL,
    champion_model TEXT NOT NULL,
    challenger_model TEXT NOT NULL,
    target_runs INT NOT NULL DEFAULT 20 CHECK (target_runs > 0),
    swap_percent INT NOT NULL DEFAULT 50 CHECK (swap_percent BETWEEN 1 AND 100),
    allow_pr_diff BOOLEAN NOT NULL DEFAULT FALSE,
    eligible_dispatches INT NOT NULL DEFAULT 0,
    lens_counters JSONB NOT NULL DEFAULT '{}'::jsonb,
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'completed', 'promoted', 'retired')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    decided_at TIMESTAMPTZ
);

CREATE UNIQUE INDEX idx_arena_trials_one_open_per_slot
    ON arena_trials (user_id, skill, lineage)
    WHERE status IN ('active', 'completed');

CREATE INDEX idx_arena_trials_user_status ON arena_trials (user_id, status, created_at DESC);

CREATE TABLE arena_rotation_state (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    skill TEXT NOT NULL CHECK (skill IN ('adversarial-review', 'plan-tournament')),
    turn_count BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (user_id, skill)
);

CREATE TABLE arena_runs (
    id BIGSERIAL PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    run_key TEXT NOT NULL,
    skill TEXT NOT NULL CHECK (skill IN ('adversarial-review', 'plan-tournament')),
    artifact_kind TEXT NOT NULL DEFAULT 'other' CHECK (artifact_kind IN ('pr_diff', 'plan', 'patch', 'branch_diff', 'other')),
    artifact_ref TEXT NOT NULL DEFAULT '',
    repo TEXT NOT NULL DEFAULT '',
    phase TEXT NOT NULL DEFAULT 'independent' CHECK (phase IN ('independent', 'targeted', 'follow_up')),
    host_model TEXT NOT NULL DEFAULT '',
    host_lineage TEXT NOT NULL DEFAULT '',
    harness TEXT NOT NULL DEFAULT '',
    machine TEXT NOT NULL DEFAULT '',
    approach_verdict TEXT NOT NULL DEFAULT '' CHECK (approach_verdict IN ('', 'keep', 'adjust', 'rethink', 'insufficient_evidence')),
    winning_role TEXT NOT NULL DEFAULT '',
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    client_sent_at TIMESTAMPTZ,
    UNIQUE (user_id, run_key)
);

CREATE INDEX idx_arena_runs_user_skill_occurred ON arena_runs (user_id, skill, occurred_at DESC);
CREATE INDEX idx_arena_runs_user_repo_occurred ON arena_runs (user_id, repo, occurred_at DESC);

CREATE TABLE arena_run_legs (
    id BIGSERIAL PRIMARY KEY,
    run_id BIGINT NOT NULL REFERENCES arena_runs(id) ON DELETE CASCADE,
    slot_index INT NOT NULL DEFAULT 0,
    role TEXT NOT NULL CHECK (role IN (
        'reviewer',
        'ponytail',
        'explore_scout',
        'minimal_diff',
        'clean_architecture',
        'edge_cases',
        'critic'
    )),
    lineage TEXT NOT NULL DEFAULT '',
    model TEXT NOT NULL,
    is_challenger BOOLEAN NOT NULL DEFAULT FALSE,
    trial_id BIGINT REFERENCES arena_trials(id) ON DELETE SET NULL,
    status TEXT NOT NULL DEFAULT 'ok' CHECK (status IN ('ok', 'retried', 'fallback', 'timeout', 'skipped')),
    duration_ms INT NOT NULL DEFAULT 0 CHECK (duration_ms >= 0), -- 0 denotes untimed / not measured
    won_core_plan BOOLEAN NOT NULL DEFAULT FALSE,
    cherry_picks_count INT NOT NULL DEFAULT 0 CHECK (cherry_picks_count >= 0),
    critic_adopted_count INT NOT NULL DEFAULT 0 CHECK (critic_adopted_count >= 0),
    critic_bloat_dropped_count INT NOT NULL DEFAULT 0 CHECK (critic_bloat_dropped_count >= 0),
    UNIQUE (run_id, slot_index)
);

CREATE INDEX idx_arena_run_legs_model ON arena_run_legs (model, role);
CREATE INDEX idx_arena_run_legs_trial ON arena_run_legs (trial_id) WHERE trial_id IS NOT NULL;

CREATE TABLE arena_candidates (
    id BIGSERIAL PRIMARY KEY,
    run_id BIGINT NOT NULL REFERENCES arena_runs(id) ON DELETE CASCADE,
    candidate_index INT NOT NULL DEFAULT 0,
    candidate_key TEXT NOT NULL,
    summary TEXT NOT NULL DEFAULT '',
    severity TEXT NOT NULL DEFAULT '',
    disposition TEXT NOT NULL CHECK (disposition IN ('adopted', 'set_aside', 'open_question')),
    caused_approach_pivot BOOLEAN NOT NULL DEFAULT FALSE,
    proposing_models TEXT[] NOT NULL DEFAULT '{}',
    human_override TEXT NOT NULL DEFAULT '' CHECK (human_override IN ('', 'confirmed', 'overturned')),
    override_note TEXT NOT NULL DEFAULT '',
    UNIQUE (run_id, candidate_key)
);

CREATE TABLE arena_coding_runs (
    id BIGSERIAL PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    run_key TEXT NOT NULL,
    model TEXT NOT NULL,
    lineage TEXT NOT NULL DEFAULT '',
    harness TEXT NOT NULL DEFAULT '',
    repo TEXT NOT NULL DEFAULT '',
    pr_number INT NOT NULL DEFAULT 0 CHECK (pr_number >= 0),
    issue_ref TEXT NOT NULL DEFAULT '',
    task_kind TEXT NOT NULL DEFAULT 'other' CHECK (task_kind IN ('feature', 'bugfix', 'refactor', 'cleanup', 'other')),
    loc_added INT NOT NULL DEFAULT 0 CHECK (loc_added >= 0),
    loc_deleted INT NOT NULL DEFAULT 0 CHECK (loc_deleted >= 0),
    size_bucket TEXT NOT NULL DEFAULT 'S' CHECK (size_bucket IN ('S', 'M', 'L')),
    files_changed INT NOT NULL DEFAULT 0 CHECK (files_changed >= 0),
    tests_delta INT NOT NULL DEFAULT 0,
    prompt_turn_count INT NOT NULL DEFAULT 0 CHECK (prompt_turn_count >= 0),
    red_ci_pushes INT NOT NULL DEFAULT 0 CHECK (red_ci_pushes >= 0),
    ccr_rounds INT NOT NULL DEFAULT 0 CHECK (ccr_rounds >= 0),
    ccr_actionable_count INT NOT NULL DEFAULT 0 CHECK (ccr_actionable_count >= 0),
    ccr_noise_count INT NOT NULL DEFAULT 0 CHECK (ccr_noise_count >= 0),
    human_threads_count INT NOT NULL DEFAULT 0 CHECK (human_threads_count >= 0),
    escaped_regression BOOLEAN NOT NULL DEFAULT FALSE,
    merged_without_followup_24h BOOLEAN NOT NULL DEFAULT FALSE,
    first_pass_clean BOOLEAN NOT NULL DEFAULT FALSE,
    abandoned BOOLEAN NOT NULL DEFAULT FALSE,
    time_to_green_sec INT NOT NULL DEFAULT 0 CHECK (time_to_green_sec >= 0),
    time_to_merge_sec INT NOT NULL DEFAULT 0 CHECK (time_to_merge_sec >= 0),
    digest_summary TEXT NOT NULL DEFAULT '',
    human_override TEXT NOT NULL DEFAULT '' CHECK (human_override IN ('', 'confirmed', 'overturned')),
    override_note TEXT NOT NULL DEFAULT '',
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    client_sent_at TIMESTAMPTZ,
    UNIQUE (user_id, run_key)
);

CREATE INDEX idx_arena_coding_runs_user_occurred ON arena_coding_runs (user_id, occurred_at DESC);
CREATE INDEX idx_arena_coding_runs_user_repo_pr ON arena_coding_runs (user_id, repo, pr_number);
