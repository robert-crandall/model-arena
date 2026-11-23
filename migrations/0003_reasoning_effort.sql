ALTER TABLE arena_roster
  ADD COLUMN IF NOT EXISTS champion_reasoning_effort TEXT NOT NULL DEFAULT '',
  ADD COLUMN IF NOT EXISTS backup_reasoning_effort   TEXT NOT NULL DEFAULT '';

ALTER TABLE arena_trials
  ADD COLUMN IF NOT EXISTS champion_reasoning_effort   TEXT NOT NULL DEFAULT '',
  ADD COLUMN IF NOT EXISTS challenger_reasoning_effort TEXT NOT NULL DEFAULT '';

ALTER TABLE arena_runs
  ADD COLUMN IF NOT EXISTS host_reasoning_effort TEXT NOT NULL DEFAULT '';

ALTER TABLE arena_run_legs
  ADD COLUMN IF NOT EXISTS reasoning_effort TEXT NOT NULL DEFAULT '';

ALTER TABLE arena_coding_runs
  ADD COLUMN IF NOT EXISTS reasoning_effort TEXT NOT NULL DEFAULT '';

CREATE INDEX IF NOT EXISTS idx_arena_run_legs_model_effort_role
  ON arena_run_legs (model, reasoning_effort, role);
