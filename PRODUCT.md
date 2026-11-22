# Model Arena (`model-arena-rust`)

Empirical AI model performance tracker, panel selector, and Challenger trial harness for multi-agent skills (`adversarial-review` and `plan-tournament`) and single-model coding PR profiles (`#5`), built on **Rust 2024 + Axum 0.8 + PostgreSQL 18 + `rmcp` stdio MCP + Solid 2.0**.

## 1. Mission & Core Loop

AI models evolve every few weeks, and prompt lenses (`minimal_diff`, `clean_architecture`, `edge_cases`) or host/judge pairings can easily mask or inflate a model's apparent quality. **Model Arena** turns subjective impressions ("Model X feels noisy lately" or "Model Y is taking forever") into an empirical, de-biased feedback loop across every machine and agent harness you use:

1. **Select**: Before `adversarial-review` or `plan-tournament` launches subagents, it calls `select_panel` (MCP tool or `POST /api/panel/select`). Model Arena returns the active Champion model for each independent lineage, rotates `plan-tournament` prompt lenses (`minimal_diff`, `clean_architecture`, `edge_cases`) in deterministic round-robin order so every lineage experiences every lens equally, and swaps in an active **Challenger** model on a bounded percentage of runs per role (`swapPercent`, default 50%).
2. **Execute**: The skill runs its subagents in parallel, measuring wall-clock duration (`durationMs`) per leg, and performs evidence-based host triage (or cross-model critic review).
3. **Record (Non-Blocking)**: At the end of the skill run, the agent calls `record_run` (`queueOnly: true` for instant local spooling, followed by a bounded background `flush_spool`). When a Challenger bout reaches its target run count (for example, 15-20 runs), `record_run` / `flush_spool` returns the completed trial in `completedTrials` so the agent can surface a brief notification in chat.
4. **Evaluate & Promote via MCP / CLI**: The web UI is strictly a **read-only observability view layer**; model promotions, trial starts, and roster updates happen exclusively via MCP tools or the CLI (`decide_trial`, `start_trial`, `set_roster`) where your explicit workflow controls live.

---

## 2. Core Capabilities

### A. Multi-Agent Tournament & Review Telemetry
- **Adversarial Review Scorecards**:
  - **Precision** (`Adopted / (Adopted + SetAside)`): Fraction of a model's proposed findings that survive host verification (or human override).
  - **Solo Catch Rate** (`SoloAdopted / Runs`): Verified findings that *only* this model caught and every other reviewer missed (excluding synthetic non-LLM lenses like `ponytail`).
  - **Approach Pivot Rate**: How often a model's finding changes the overall review verdict (`keep` -> `adjust` / `rethink`).
- **Plan Tournament Scorecards (De-Biased)**:
  - **Raw & Lens-Adjusted Core Plan Win Rate**: Macro-averaged across `minimal_diff`, `clean_architecture`, and `edge_cases` (`1/3 * sum(win_rate_in_lens)`) so an uneven lens assignment never skews a model's score.
  - **Cherry-Picks / Run**: Distinct ideas rescued from a planner leg even when another leg won the core structure.
  - **Critic Value & Bloat Rates**: Amendments adopted from the cross-model critic vs. speculative critic additions dropped as bloat.
- **Interaction Matrices**:
  - **Model x Prompt Lens Matrix**: Exposes whether a model excels as an edge-case hunter (`edge_cases`) or minimal-diff planner (`minimal_diff`).
  - **Host Judge x Reviewer/Planner Matrix**: Exposes self-preference bias when a host model judges its own lineage vs. other lineages.

### B. Latency & Speed-vs-Quality Tradeoffs (`#4`)
- **Per-Role Latency Percentiles (`RoleLatencyStats`)**:
  - Tracks `runs`, `avgDurationMs`, `p50DurationMs`, and `p90DurationMs` per `(model, skill, role)` so planner lenses and reviewer passes are never mixed in a single average.
- **Parallel Panel Bottleneck (`stragglerRate`)**:
  - Because `adversarial-review` and `plan-tournament` wait for the slowest parallel subagent leg before synthesizing, `stragglerRate` measures the fraction of multi-leg runs (with `>= 2` timed competing legs) where a model was the slowest leg holding up the entire panel.
- **Speed-Weighed Trial Verdicts**:
  - Challenger trial verdicts compare both quality (`soloCatchRate`, `precision`, `adjustedWinRate`, `contributionRate`) and speed (`p50DurationMs` / `avgDurationMs`, `stragglerRate`):
    - **Fast Parity Promotion**: A Challenger with equal quality (`rateDelta >= -0.02` and `scoreDelta >= -0.02`) and `>= 30%` lower latency (`challengerMs <= championMs * 0.70`) earns `promote`.
    - **Latency Bottleneck Guard**: A Challenger with only marginal quality gain (`< +0.08`) that is `>= 2x` slower than the incumbent Champion is flagged as `close_call` with a latency warning rather than auto-recommended for promotion.

### C. Single-Model Coding PR Profiles (`#5`)
- Tracks end-to-end coding agent execution on real pull requests (`arena_coding_runs`), normalized by **Diff Size Bucket** (`S`: `< 50` LOC, `M`: `50..=250` LOC, `L`: `> 250` LOC) and **Task Kind** (`feature`, `bugfix`, `refactor`, `cleanup`, `other`):
  - **Autonomy**: `promptTurnCount` (user steering turns after initial prompt).
  - **Review Cleanliness**: `firstPassClean`, `ccrRounds`, `ccrActionableCount`, `ccrNoiseCount`, `humanThreadsCount`.
  - **Velocity & CI Discipline**: `timeToGreenSec`, `timeToMergeSec`, `redCiPushes`.
  - **Restraint**: `locAdded`, `locDeleted`, `filesChanged`, `testsDelta`.
  - **Merged Outcome**: `mergedWithoutFollowup24h`, `escapedRegression`, `abandoned`.

### D. Human Ground-Truth Overrides
- Host triage is a fast proxy, not infallible ground truth. Whenever an adopted finding turns out to be a false positive or a set-aside warning turns out to be a real bug, `override_candidate` (`POST /api/candidates/{id}/override`) or `override_coding_run` (`POST /api/coding-runs/{id}/override`) records `confirmed` or `overturned` and immediately recalculates precision, solo catches, and coding cleanliness scores. Replaying a run preserves existing human overrides.

### E. Read-Only Web UI Charter
- The Solid 2.0 web interface is a **read-only observability dashboard**.
- It supports selectable date ranges (`7d`, `30d`, `90d`, `All`, or custom `from` / `to` timestamps), repository and skill filters, 3-agent panel seat fit cards, Champion vs. Challenger bout comparisons, latency & straggler distributions, Model x Lens / Host x Reviewer matrices, and single-model coding PR profiles.
- **No model promotion or roster mutation buttons exist in the web UI.** All mutations (`set_roster`, `start_trial`, `decide_trial`, `record_run`, `override_candidate`) are performed through authenticated API/MCP/CLI workflows.
