-- Paired Champion vs Challenger shadow bouts for coding and orchestration.

CREATE TABLE arena_shadow_bouts (
    id BIGSERIAL PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    bout_key TEXT NOT NULL,
    track TEXT NOT NULL CHECK (track IN ('coding', 'orchestration')),
    champion_source TEXT NOT NULL DEFAULT 'headless' CHECK (champion_source IN ('headless', 'existing_ref')),
    champion_human_turns INT NOT NULL DEFAULT 0 CHECK (champion_human_turns >= 0),
    repo TEXT NOT NULL DEFAULT '',
    base_sha TEXT NOT NULL DEFAULT '',
    task_ref TEXT NOT NULL DEFAULT '',
    task_kind TEXT NOT NULL DEFAULT 'other' CHECK (task_kind IN ('feature', 'bugfix', 'refactor', 'cleanup', 'other')),
    prompt_sha256 TEXT NOT NULL DEFAULT '',
    champion_impl_champion_test TEXT NOT NULL DEFAULT 'skipped'
        CHECK (champion_impl_champion_test IN ('pass', 'fail', 'compile_error', 'no_tests', 'timeout', 'skipped')),
    challenger_impl_challenger_test TEXT NOT NULL DEFAULT 'skipped'
        CHECK (challenger_impl_challenger_test IN ('pass', 'fail', 'compile_error', 'no_tests', 'timeout', 'skipped')),
    champion_impl_challenger_test TEXT NOT NULL DEFAULT 'skipped'
        CHECK (champion_impl_challenger_test IN ('pass', 'fail', 'compile_error', 'no_tests', 'timeout', 'skipped')),
    challenger_impl_champion_test TEXT NOT NULL DEFAULT 'skipped'
        CHECK (challenger_impl_champion_test IN ('pass', 'fail', 'compile_error', 'no_tests', 'timeout', 'skipped')),
    winner TEXT NOT NULL DEFAULT '' CHECK (winner IN ('', 'champion', 'challenger', 'tie', 'both_failed')),
    judge_model TEXT NOT NULL DEFAULT '',
    judge_lineage TEXT NOT NULL DEFAULT '',
    judge_rationale TEXT NOT NULL DEFAULT '',
    judged_at TIMESTAMPTZ,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    client_sent_at TIMESTAMPTZ,
    UNIQUE (user_id, bout_key)
);

CREATE INDEX idx_arena_shadow_bouts_user_track_occurred
    ON arena_shadow_bouts (user_id, track, occurred_at DESC);

CREATE TABLE arena_shadow_bout_legs (
    id BIGSERIAL PRIMARY KEY,
    bout_id BIGINT NOT NULL REFERENCES arena_shadow_bouts(id) ON DELETE CASCADE,
    side TEXT NOT NULL CHECK (side IN ('champion', 'challenger')),
    model TEXT NOT NULL,
    reasoning_effort TEXT NOT NULL DEFAULT '',
    lineage TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'ok' CHECK (status IN ('ok', 'timeout', 'credit_cap', 'failed', 'imported')),
    duration_ms BIGINT NOT NULL DEFAULT 0 CHECK (duration_ms >= 0),
    loc_added INT NOT NULL DEFAULT 0 CHECK (loc_added >= 0),
    loc_deleted INT NOT NULL DEFAULT 0 CHECK (loc_deleted >= 0),
    files_changed INT NOT NULL DEFAULT 0 CHECK (files_changed >= 0),
    tests_delta INT NOT NULL DEFAULT 0,
    usage_available BOOLEAN NOT NULL DEFAULT FALSE,
    nano_aiu BIGINT NOT NULL DEFAULT 0 CHECK (nano_aiu >= 0),
    premium_requests DOUBLE PRECISION NOT NULL DEFAULT 0 CHECK (premium_requests >= 0),
    input_tokens BIGINT NOT NULL DEFAULT 0 CHECK (input_tokens >= 0),
    output_tokens BIGINT NOT NULL DEFAULT 0 CHECK (output_tokens >= 0),
    turn_count INT NOT NULL DEFAULT 0 CHECK (turn_count >= 0),
    subagent_spawns INT NOT NULL DEFAULT 0 CHECK (subagent_spawns >= 0),
    tool_errors INT NOT NULL DEFAULT 0 CHECK (tool_errors >= 0),
    metrics_json JSONB NOT NULL DEFAULT '{}'::jsonb,
    UNIQUE (bout_id, side)
);

CREATE INDEX idx_arena_shadow_bout_legs_bout ON arena_shadow_bout_legs (bout_id);
CREATE INDEX idx_arena_shadow_bout_legs_model ON arena_shadow_bout_legs (model, reasoning_effort);
