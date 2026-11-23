import {
  createMemo,
  createSignal,
  For,
  Match,
  onSettled,
  Show,
  Switch,
} from "solid-js";
import {
  api,
  type ApiToken,
  type CodingLeaderboard,
  type CreatedApiToken,
  type Leaderboard,
  type ModelScorecard,
  type User,
} from "../lib/api/client";
import { useAuth } from "../lib/auth";
import { ThemePicker } from "./ThemePicker";

type ArenaTab = "review" | "tournament" | "coding" | "trials" | "history";

function formatPct(val: number): string {
  return `${(val * 100).toFixed(1)}%`;
}

function formatDurationMs(ms: number): string {
  if (!ms || ms <= 0) return "—";
  if (ms < 1000) return `${ms}ms`;
  return `${(ms / 1000).toFixed(1)}s`;
}

function formatSeconds(sec: number): string {
  if (!sec || sec <= 0) return "—";
  if (sec < 60) return `${sec}s`;
  return `${(sec / 60).toFixed(1)}m`;
}

function formatModelLabel(model: string, effort?: string): string {
  return effort ? `${model} (${effort})` : model;
}

export function AppShell(props: { user: User }) {
  const auth = useAuth();
  const [activeTab, setActiveTab] = createSignal<ArenaTab>("review");
  const [daysWindow, setDaysWindow] = createSignal<number>(90);
  const [repoFilter, setRepoFilter] = createSignal<string>("");
  const [arenaLoading, setArenaLoading] = createSignal(false);
  const [arenaError, setArenaError] = createSignal<string | null>(null);
  const [leaderboard, setLeaderboard] = createSignal<Leaderboard | null>(null);
  const [codingBoard, setCodingBoard] = createSignal<CodingLeaderboard | null>(
    null,
  );


  const [tokensLoaded, setTokensLoaded] = createSignal(false);
  const [tokens, setTokens] = createSignal<ApiToken[]>([]);
  const [tokenName, setTokenName] = createSignal("");
  const [createdToken, setCreatedToken] = createSignal<CreatedApiToken | null>(
    null,
  );
  const [tokenError, setTokenError] = createSignal<string | null>(null);
  const [logoutError, setLogoutError] = createSignal<string | null>(null);
  const [creating, setCreating] = createSignal(false);
  let tokenLoadSeq = 0;
  let arenaLoadSeq = 0;

  async function loadArenaData(days = daysWindow(), repo = repoFilter()) {
    const seq = ++arenaLoadSeq;
    setArenaLoading(true);
    setArenaError(null);
    const trimmedRepo = repo.trim() || undefined;
    try {
      const [lbRes, codingRes] = await Promise.all([
        api.GET("/api/leaderboard", {
          params: {
            query: {
              days,
              repo: trimmedRepo,
            },
          },
        }),
        api.GET("/api/coding-leaderboard", {
          params: {
            query: {
              days,
              repo: trimmedRepo,
            },
          },
        }),
      ]);
      if (seq !== arenaLoadSeq) return;
      if (lbRes.error || !lbRes.data) {
        setLeaderboard(null);
        setCodingBoard(null);
        setArenaError(
          lbRes.error?.error ?? "Could not load leaderboard telemetry.",
        );
        return;
      }
      if (codingRes.error || !codingRes.data) {
        setLeaderboard(null);
        setCodingBoard(null);
        setArenaError(
          codingRes.error?.error ??
            "Could not load coding profile leaderboard.",
        );
        return;
      }
      setLeaderboard(lbRes.data);
      setCodingBoard(codingRes.data);
    } catch {
      if (seq !== arenaLoadSeq) return;
      setLeaderboard(null);
      setCodingBoard(null);
      setArenaError("Could not reach the server to load arena telemetry.");
    } finally {
      if (seq === arenaLoadSeq) {
        setArenaLoading(false);
      }
    }
  }

  onSettled(() => {
    void loadArenaData();
  });

  const reviewCards = createMemo<ModelScorecard[]>(() =>
    (leaderboard()?.models ?? []).filter(
      (c: ModelScorecard) => c.reviewRuns > 0,
    ),
  );

  const tournamentCards = createMemo<ModelScorecard[]>(() =>
    (leaderboard()?.models ?? []).filter(
      (c: ModelScorecard) => c.plannerRuns > 0 || c.criticRuns > 0,
    ),
  );


  async function handleLogout() {
    setLogoutError(null);
    const result = await auth.logout();
    if (result.error) {
      setLogoutError(result.error);
    }
  }

  async function loadTokens() {
    const seq = ++tokenLoadSeq;
    setTokenError(null);
    try {
      const { data, error } = await api.GET("/api/tokens");
      if (seq !== tokenLoadSeq) return;
      if (error || !data) {
        setTokenError(
          error?.error ?? "Could not load personal access tokens.",
        );
        return;
      }
      setTokens(data);
      setTokensLoaded(true);
    } catch {
      if (seq !== tokenLoadSeq) return;
      setTokenError(
        "Could not reach the server to load personal access tokens.",
      );
    }
  }

  function handleDisclosureToggle(
    e: Event & { currentTarget: HTMLDetailsElement },
  ) {
    if (e.currentTarget.open) {
      void loadTokens();
    }
  }

  async function handleCreateToken(e: SubmitEvent) {
    e.preventDefault();
    const trimmed = tokenName().trim();
    if (!trimmed) return;

    setCreating(true);
    setTokenError(null);
    try {
      const { data, error } = await api.POST("/api/tokens", {
        body: { name: trimmed },
      });
      if (error || !data) {
        setTokenError(error?.error ?? "Could not create token.");
        return;
      }
      setCreatedToken(data);
      setTokenName("");
      setTokens((prev) => [
        {
          id: data.id,
          name: data.name,
          last4: data.last4,
          createdAt: data.createdAt,
          lastUsedAt: null,
        },
        ...prev.filter((item) => item.id !== data.id),
      ]);
      setTokensLoaded(true);
      await loadTokens();
    } catch {
      setTokenError(
        "Could not reach the server while minting the token. Reopen this panel to check existing tokens before retrying.",
      );
    } finally {
      setCreating(false);
    }
  }

  async function handleRevokeToken(id: string) {
    setTokenError(null);
    try {
      const { error } = await api.DELETE("/api/tokens/{id}", {
        params: { path: { id } },
      });
      if (error) {
        setTokenError(error.error);
        return;
      }
      ++tokenLoadSeq;
      setTokens((prev) => prev.filter((item) => item.id !== id));
      if (createdToken()?.id === id) {
        setCreatedToken(null);
      }
    } catch {
      setTokenError("Could not reach the server to revoke that token.");
    }
  }

  function handleDaysChange(nextDays: number) {
    setDaysWindow(nextDays);
    void loadArenaData(nextDays, repoFilter());
  }

  function handleFilterSubmit(e: SubmitEvent) {
    e.preventDefault();
    void loadArenaData(daysWindow(), repoFilter());
  }

  const displayName = () => props.user.name || props.user.email;

  return (
    <div class="page-shell">
      <header class="topbar">
        <div class="topbar-inner">
          <div class="brand">
            <img class="brand-icon" src="/icon.svg" alt="" />
            <span>Model Arena</span>
          </div>

          <div class="topbar-actions">
            <span class="user-pill" title={props.user.email}>
              {displayName()}
            </span>
            <ThemePicker />
            <button
              type="button"
              class="btn btn-secondary"
              onClick={() => void handleLogout()}
            >
              Sign out
            </button>
          </div>
        </div>
      </header>

      <main class="main-container">
        <Show when={logoutError()}>
          {(msg) => (
            <div class="alert-error" role="alert">
              {msg()}
            </div>
          )}
        </Show>

        <section class="surface-card" aria-labelledby="focus-heading">
          <div class="arena-hero-top">
            <div>
              <span class="eyebrow">Read-only telemetry view</span>
              <h1 id="focus-heading">Ready for Model Arena</h1>
              <p class="lead-copy">
                Signed in as <strong>{displayName()}</strong>. View empirical
                subagent review catch rates, latency percentiles, de-biased plan
                tournament win rates, Challenger bout progress, and single-model
                coding profiles. Roster and trial mutations stay in{" "}
                <code>model-arena-rust-mcp</code>.
              </p>
            </div>

            <form class="arena-filter-bar" onSubmit={handleFilterSubmit}>
              <div class="field">
                <label for="arena-days-select">Window</label>
                <select
                  id="arena-days-select"
                  class="select"
                  aria-label="Lookback window"
                  value={String(daysWindow())}
                  onChange={(e) =>
                    handleDaysChange(Number(e.currentTarget.value))
                  }
                >
                  <option value="30">Last 30 days</option>
                  <option value="90">Last 90 days</option>
                  <option value="365">Last 365 days</option>
                </select>
              </div>

              <div class="field">
                <label for="arena-repo-filter">Repository</label>
                <input
                  id="arena-repo-filter"
                  class="input"
                  type="text"
                  placeholder="All repos (e.g. owner/repo)"
                  value={repoFilter()}
                  onInput={(e) => setRepoFilter(e.currentTarget.value)}
                />
              </div>

              <button
                type="submit"
                class="btn btn-secondary"
                disabled={arenaLoading()}
              >
                {arenaLoading() ? "Refreshing..." : "Refresh"}
              </button>
            </form>
          </div>

          <Show when={leaderboard()}>
            {(lb) => (
              <div class="kpi-strip" aria-label="Arena summary metrics">
                <div class="kpi-pill">
                  <span class="kpi-label">Multi-model runs</span>
                  <strong class="kpi-value">{lb().totalRuns}</strong>
                </div>
                <div class="kpi-pill">
                  <span class="kpi-label">Tracked models</span>
                  <strong class="kpi-value">{lb().models.length}</strong>
                </div>
                <div class="kpi-pill">
                  <span class="kpi-label">Challenger bouts</span>
                  <strong class="kpi-value">{lb().trials.length}</strong>
                </div>
                <div class="kpi-pill">
                  <span class="kpi-label">Coding &amp; PR runs</span>
                  <strong class="kpi-value">
                    {codingBoard()?.totalRuns ?? 0}
                  </strong>
                </div>
              </div>
            )}
          </Show>
        </section>

        <Show when={arenaError()}>
          {(msg) => (
            <div class="alert-error" role="alert">
              {msg()}
            </div>
          )}
        </Show>

        <section class="surface-card" aria-label="Model Arena views">
          <nav class="arena-tabs" aria-label="Telemetry views">
            <button
              type="button"
              class={["tab-btn", { active: activeTab() === "review" }]}
              aria-pressed={activeTab() === "review" ? "true" : "false"}
              onClick={() => setActiveTab("review")}
            >
              Adversarial Review
            </button>
            <button
              type="button"
              class={["tab-btn", { active: activeTab() === "tournament" }]}
              aria-pressed={activeTab() === "tournament" ? "true" : "false"}
              onClick={() => setActiveTab("tournament")}
            >
              Plan Tournament
            </button>
            <button
              type="button"
              class={["tab-btn", { active: activeTab() === "coding" }]}
              aria-pressed={activeTab() === "coding" ? "true" : "false"}
              onClick={() => setActiveTab("coding")}
            >
              Coding &amp; PRs
            </button>
            <button
              type="button"
              class={["tab-btn", { active: activeTab() === "trials" }]}
              aria-pressed={activeTab() === "trials" ? "true" : "false"}
              onClick={() => setActiveTab("trials")}
            >
              Challenger Trials
            </button>
            <button
              type="button"
              class={["tab-btn", { active: activeTab() === "history" }]}
              aria-pressed={activeTab() === "history" ? "true" : "false"}
              onClick={() => setActiveTab("history")}
            >
              Matrices &amp; History
            </button>
          </nav>

          <Switch>
            <Match when={activeTab() === "review"}>
              <div class="arena-panel">
                <h2>Adversarial review scorecards &amp; latency</h2>
                <p class="field-hint">
                  Sorted by solo catch rate, precision, and catches per minute.
                  Includes subagent wall-clock p50/p90 latency and straggler
                  rate.
                </p>
                <Show
                  when={reviewCards().length > 0}
                  fallback={
                    <p class="field-hint">
                      No adversarial-review runs recorded in this window yet.
                    </p>
                  }
                >
                  <div class="table-scroll">
                    <table class="arena-table">
                      <thead>
                        <tr>
                          <th>Model</th>
                          <th>Lineage</th>
                          <th>Runs</th>
                          <th>Solo Catch Rate</th>
                          <th>Precision</th>
                          <th>Catches (Solo / Shared)</th>
                          <th>p50 / p90</th>
                          <th>Straggler</th>
                          <th>Catches/min</th>
                        </tr>
                      </thead>
                      <tbody>
                        <For each={reviewCards()}>
                          {(card) => (
                            <tr>
                              <td>
                                <code>
                                  {formatModelLabel(
                                    card.model,
                                    card.reasoningEffort,
                                  )}
                                </code>
                              </td>
                              <td>{card.lineage}</td>
                              <td>{card.reviewRuns}</td>
                              <td>{formatPct(card.soloCatchRate)}</td>
                              <td>{formatPct(card.precision)}</td>
                              <td>
                                {card.soloCatches} / {card.sharedCatches}
                              </td>
                              <td>
                                {formatDurationMs(card.p50DurationMs)} /{" "}
                                {formatDurationMs(card.p90DurationMs)}
                              </td>
                              <td>{formatPct(card.stragglerRate)}</td>
                              <td>{card.catchesPerMinute.toFixed(2)}</td>
                            </tr>
                          )}
                        </For>
                      </tbody>
                    </table>
                  </div>
                </Show>
              </div>
            </Match>

            <Match when={activeTab() === "tournament"}>
              <div class="arena-panel">
                <h2>Plan tournament scorecards</h2>
                <p class="field-hint">
                  De-biased tournament score, core plan wins, cherry-picks,
                  critic adoption, and planner latency across lens rotations.
                </p>
                <Show
                  when={tournamentCards().length > 0}
                  fallback={
                    <p class="field-hint">
                      No plan-tournament runs recorded in this window yet.
                    </p>
                  }
                >
                  <div class="table-scroll">
                    <table class="arena-table">
                      <thead>
                        <tr>
                          <th>Model</th>
                          <th>Lineage</th>
                          <th>Planner Runs</th>
                          <th>De-Biased Score</th>
                          <th>Core Wins</th>
                          <th>Cherry-Picks</th>
                          <th>Critic Adopted</th>
                          <th>p50 / p90</th>
                        </tr>
                      </thead>
                      <tbody>
                        <For each={tournamentCards()}>
                          {(card) => (
                            <tr>
                              <td>
                                <code>
                                  {formatModelLabel(
                                    card.model,
                                    card.reasoningEffort,
                                  )}
                                </code>
                              </td>
                              <td>{card.lineage}</td>
                              <td>{card.plannerRuns}</td>
                              <td>
                                {formatPct(card.debiasedTournamentScore)}
                              </td>
                              <td>{card.corePlanWins}</td>
                              <td>{card.cherryPicks}</td>
                              <td>{card.criticAdopted}</td>
                              <td>
                                {formatDurationMs(card.p50DurationMs)} /{" "}
                                {formatDurationMs(card.p90DurationMs)}
                              </td>
                            </tr>
                          )}
                        </For>
                      </tbody>
                    </table>
                  </div>
                </Show>
              </div>
            </Match>

            <Match when={activeTab() === "coding"}>
              <div class="arena-panel">
                <h2>Single-model coding &amp; PR profiles</h2>
                <p class="field-hint">
                  First-pass clean rate, zero-touch merge rate, CCR review
                  rounds, red CI pushes, and time-to-merge percentiles.
                </p>
                <Show
                  when={(codingBoard()?.models ?? []).length > 0}
                  fallback={
                    <p class="field-hint">
                      No single-model coding runs recorded in this window yet.
                    </p>
                  }
                >
                  <div class="table-scroll">
                    <table class="arena-table">
                      <thead>
                        <tr>
                          <th>Model</th>
                          <th>Lineage</th>
                          <th>Runs</th>
                          <th>First-Pass Clean</th>
                          <th>Zero-Touch</th>
                          <th>Avg CCR Rounds</th>
                          <th>Avg Red CI</th>
                          <th>24h Clean Merge</th>
                          <th>p50 / p90 Merge</th>
                        </tr>
                      </thead>
                      <tbody>
                        <For each={codingBoard()?.models ?? []}>
                          {(card) => (
                            <tr>
                              <td>
                                <code>
                                  {formatModelLabel(
                                    card.model,
                                    card.reasoningEffort,
                                  )}
                                </code>
                              </td>
                              <td>{card.lineage}</td>
                              <td>{card.totalRuns}</td>
                              <td>{formatPct(card.firstPassCleanRate)}</td>
                              <td>{formatPct(card.zeroTouchRate)}</td>
                              <td>{card.avgCcrRounds.toFixed(2)}</td>
                              <td>{card.avgRedCiPushes.toFixed(2)}</td>
                              <td>{formatPct(card.mergedClean24hRate)}</td>
                              <td>
                                {formatSeconds(card.p50TimeToMergeSec)} /{" "}
                                {formatSeconds(card.p90TimeToMergeSec)}
                              </td>
                            </tr>
                          )}
                        </For>
                      </tbody>
                    </table>
                  </div>
                </Show>
              </div>
            </Match>

            <Match when={activeTab() === "trials"}>
              <div class="arena-panel">
                <h2>Challenger trials &amp; active roster (view-only)</h2>
                <p class="field-hint">
                  Speed- and quality-weighed trial verdicts. Promotions and
                  retirements are executed exclusively via{" "}
                  <code>model-arena-rust-mcp</code>.
                </p>
                <Show
                  when={(leaderboard()?.trials ?? []).length > 0}
                  fallback={
                    <p class="field-hint">
                      No Challenger bouts recorded yet.
                    </p>
                  }
                >
                  <div class="table-scroll">
                    <table class="arena-table">
                      <thead>
                        <tr>
                          <th>Trial</th>
                          <th>Skill / Lineage</th>
                          <th>Champion vs Challenger</th>
                          <th>Progress</th>
                          <th>Precision (Champ / Chal)</th>
                          <th>p50 Latency</th>
                          <th>Verdict Hint</th>
                        </tr>
                      </thead>
                      <tbody>
                        <For each={leaderboard()?.trials ?? []}>
                          {(item) => (
                            <tr>
                              <td>
                                <code>#{item.trial.id}</code> ({item.trial.status}
                                )
                              </td>
                              <td>
                                {item.trial.skill} / {item.trial.lineage}
                              </td>
                              <td>
                                <code>
                                  {formatModelLabel(
                                    item.trial.championModel,
                                    item.trial.championReasoningEffort,
                                  )}
                                </code>{" "}
                                vs{" "}
                                <code>
                                  {formatModelLabel(
                                    item.trial.challengerModel,
                                    item.trial.challengerReasoningEffort,
                                  )}
                                </code>
                              </td>
                              <td>
                                {item.trial.completedRuns} /{" "}
                                {item.trial.targetRuns}
                              </td>
                              <td>
                                {formatPct(item.champion.precision)} /{" "}
                                {formatPct(item.challenger.precision)}
                              </td>
                              <td>
                                {formatDurationMs(item.champion.p50DurationMs)}{" "}
                                /{" "}
                                {formatDurationMs(
                                  item.challenger.p50DurationMs,
                                )}
                              </td>
                              <td>
                                <span class="badge-pill">
                                  {item.verdictHint}
                                </span>
                                <div class="field-hint">
                                  {item.verdictNotes}
                                </div>
                              </td>
                            </tr>
                          )}
                        </For>
                      </tbody>
                    </table>
                  </div>
                </Show>

                <Show when={(leaderboard()?.roster ?? []).length > 0}>
                  <h3>Configured lineage roster</h3>
                  <div class="table-scroll">
                    <table class="arena-table">
                      <thead>
                        <tr>
                          <th>Skill</th>
                          <th>Lineage</th>
                          <th>Champion</th>
                          <th>Backup</th>
                          <th>Status</th>
                        </tr>
                      </thead>
                      <tbody>
                        <For each={leaderboard()?.roster ?? []}>
                          {(slot) => (
                            <tr>
                              <td>{slot.skill}</td>
                              <td>{slot.lineage}</td>
                              <td>
                                <code>
                                  {formatModelLabel(
                                    slot.championModel,
                                    slot.championReasoningEffort,
                                  )}
                                </code>
                              </td>
                              <td>
                                <code>
                                  {slot.backupModel
                                    ? formatModelLabel(
                                        slot.backupModel,
                                        slot.backupReasoningEffort,
                                      )
                                    : "—"}
                                </code>
                              </td>
                              <td>{slot.enabled ? "Enabled" : "Disabled"}</td>
                            </tr>
                          )}
                        </For>
                      </tbody>
                    </table>
                  </div>
                </Show>
              </div>
            </Match>

            <Match when={activeTab() === "history"}>
              <div class="arena-panel">
                <h2>Lens matrix, host affinity &amp; recent runs</h2>
                <Show when={(leaderboard()?.lensMatrix ?? []).length > 0}>
                  <h3>Plan-tournament lens × model matrix</h3>
                  <div class="table-scroll">
                    <table class="arena-table">
                      <thead>
                        <tr>
                          <th>Model</th>
                          <th>Lens</th>
                          <th>Runs</th>
                          <th>Core Wins</th>
                          <th>Win Rate</th>
                          <th>Cherry-Picks</th>
                        </tr>
                      </thead>
                      <tbody>
                        <For each={leaderboard()?.lensMatrix ?? []}>
                          {(cell) => (
                            <tr>
                              <td>
                                <code>
                                  {formatModelLabel(
                                    cell.model,
                                    cell.reasoningEffort,
                                  )}
                                </code>
                              </td>
                              <td>{cell.lens}</td>
                              <td>{cell.runs}</td>
                              <td>{cell.wins}</td>
                              <td>{formatPct(cell.winRate)}</td>
                              <td>{cell.cherryPicks}</td>
                            </tr>
                          )}
                        </For>
                      </tbody>
                    </table>
                  </div>
                </Show>

                <Show when={(leaderboard()?.hostBiasMatrix ?? []).length > 0}>
                  <h3>Host judge × proposer adoption matrix</h3>
                  <div class="table-scroll">
                    <table class="arena-table">
                      <thead>
                        <tr>
                          <th>Host</th>
                          <th>Reviewer Model</th>
                          <th>Evaluated</th>
                          <th>Adopted</th>
                          <th>Adoption Rate</th>
                        </tr>
                      </thead>
                      <tbody>
                        <For each={leaderboard()?.hostBiasMatrix ?? []}>
                          {(cell) => (
                            <tr>
                              <td>
                                <code>
                                  {formatModelLabel(
                                    cell.hostModel,
                                    cell.hostReasoningEffort,
                                  )}
                                </code>{" "}
                                <span class="field-hint">
                                  ({cell.hostLineage})
                                </span>
                              </td>
                              <td>
                                <code>
                                  {formatModelLabel(
                                    cell.reviewerModel,
                                    cell.reviewerReasoningEffort,
                                  )}
                                </code>
                              </td>
                              <td>{cell.candidatesEvaluated}</td>
                              <td>{cell.adoptedCount}</td>
                              <td>{formatPct(cell.adoptionRate)}</td>
                            </tr>
                          )}
                        </For>
                      </tbody>
                    </table>
                  </div>
                </Show>

                <h3>Recent multi-model runs</h3>
                <Show
                  when={(leaderboard()?.recentRuns ?? []).length > 0}
                  fallback={<p class="field-hint">No recent runs recorded.</p>}
                >
                  <div class="table-scroll">
                    <table class="arena-table">
                      <thead>
                        <tr>
                          <th>Run Key</th>
                          <th>Skill</th>
                          <th>Artifact</th>
                          <th>Outcome</th>
                          <th>Candidates</th>
                        </tr>
                      </thead>
                      <tbody>
                        <For each={leaderboard()?.recentRuns ?? []}>
                          {(run) => (
                            <tr>
                              <td>
                                <code>{run.runKey}</code>
                              </td>
                              <td>{run.skill}</td>
                              <td>{run.artifactRef || run.artifactKind}</td>
                              <td>
                                {run.approachVerdict || run.winningRole || "—"}
                              </td>
                              <td>{run.candidates.length}</td>
                            </tr>
                          )}
                        </For>
                      </tbody>
                    </table>
                  </div>
                </Show>
              </div>
            </Match>
          </Switch>
        </section>


        <details class="disclosure" onToggle={handleDisclosureToggle}>
          <summary>
            <span>AI &amp; API access</span>
            <span class="disclosure-hint">
              Personal access tokens for model-arena-rust-mcp
            </span>
          </summary>

          <div class="disclosure-body">
            <p class="lead-copy">
              Run <code>make mcp-token</code> on your workstation or mint a
              personal access token below for <code>model-arena-rust-mcp</code>.
            </p>

            <Show when={tokenError()}>
              {(msg) => (
                <div class="alert-error" role="alert">
                  {msg()}
                </div>
              )}
            </Show>

            <Show when={createdToken()}>
              {(minted) => (
                <div class="secret-banner" role="status">
                  <strong>Copy your new token now (shown once):</strong>
                  <div class="secret-row">
                    <code class="secret-code">{minted().token}</code>
                  </div>
                  <p>
                    Token <strong>{minted().name}</strong> ending in{" "}
                    <code>{minted().last4}</code>.
                  </p>
                </div>
              )}
            </Show>

            <form
              class="inline-form"
              onSubmit={(e) => void handleCreateToken(e)}
            >
              <div class="field">
                <label for="new-token-name">New token name</label>
                <input
                  id="new-token-name"
                  class="input"
                  type="text"
                  placeholder="e.g. laptop-mcp"
                  required
                  maxlength={80}
                  value={tokenName()}
                  onInput={(e) => setTokenName(e.currentTarget.value)}
                />
              </div>
              <button
                class="btn btn-primary"
                type="submit"
                disabled={creating()}
              >
                {creating() ? "Minting..." : "Mint token"}
              </button>
            </form>

            <Show
              when={tokens().length > 0}
              fallback={
                <Show when={tokensLoaded()}>
                  <p class="field-hint">
                    No personal access tokens minted for this account yet.
                  </p>
                </Show>
              }
            >
              <ul class="token-list" aria-label="Personal access tokens">
                <For each={tokens()}>
                  {(tok) => (
                    <li class="token-item">
                      <div class="token-meta">
                        <span class="token-name">{tok.name}</span>
                        <span class="token-detail">
                          Ends in <code>{tok.last4}</code>
                        </span>
                      </div>
                      <button
                        type="button"
                        class="btn btn-danger"
                        onClick={() => void handleRevokeToken(tok.id)}
                      >
                        Revoke
                      </button>
                    </li>
                  )}
                </For>
              </ul>
            </Show>
          </div>
        </details>
      </main>
    </div>
  );
}

