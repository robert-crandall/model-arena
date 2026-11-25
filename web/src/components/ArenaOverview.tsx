import { createMemo, For, Show } from "solid-js";
import type { CodingLeaderboard, Leaderboard } from "../lib/api/client";
import {
  buildLensHeatmap,
  buildScatterPlot,
  buildStrengthProfiles,
} from "../lib/visuals";

export function ArenaOverview(props: {
  leaderboard: Leaderboard | null;
  codingBoard: CodingLeaderboard | null;
}) {
  const scatter = createMemo(() =>
    buildScatterPlot(props.leaderboard?.models ?? []),
  );

  const profiles = createMemo(() =>
    buildStrengthProfiles(
      props.leaderboard?.models ?? [],
      props.codingBoard?.models ?? [],
    ),
  );

  const heatmap = createMemo(() =>
    buildLensHeatmap(
      props.leaderboard?.lensMatrix ?? [],
      props.leaderboard?.models ?? [],
    ),
  );

  const hasAnyData = createMemo(
    () =>
      scatter().points.length > 0 ||
      profiles().length > 0 ||
      heatmap().rows.length > 0,
  );

  return (
    <div class="arena-panel overview-stack">
      <Show
        when={hasAnyData()}
        fallback={
          <p class="field-hint">
            No model telemetry recorded in this window yet. Run{" "}
            <code>adversarial-review</code>, <code>plan-tournament</code>, or
            record a coding PR run to populate the visual overview.
          </p>
        }
      >
        {/* 1. Adversarial Review Quadrant */}
        <Show when={scatter().points.length > 0}>
          <section class="visual-card" aria-labelledby="quadrant-heading">
            <div class="visual-card-head">
              <div>
                <h2 id="quadrant-heading">
                  Adversarial review: bug hunting vs. signal quality
                </h2>
                <p class="field-hint">
                  Bubble size = review runs (n). Dashed ring = early sample (n
                  &lt; 10). Top-right is ideal: finds unique bugs without noisy
                  false alarms.
                </p>
              </div>
              <div class="chart-legend" aria-label="Model lineages">
                <For each={scatter().lineages}>
                  {(lineage) => (
                    <span class="chart-legend-item">
                      <span
                        class="lineage-dot"
                        style={{ background: lineage.colorVar }}
                      />
                      {lineage.label}
                    </span>
                  )}
                </For>
              </div>
            </div>

            <div class="scatter-wrap">
              <svg
                class="scatter-svg"
                viewBox={`0 0 ${scatter().width} ${scatter().height}`}
                role="img"
                aria-label="Scatter plot comparing reviewer solo catch rate against precision"
              >
                {/* Horizontal grid lines */}
                <For each={scatter().yTicks}>
                  {(tick) => (
                    <g>
                      <line
                        class="chart-grid-line"
                        x1={scatter().plotLeft}
                        y1={tick.y}
                        x2={scatter().plotRight}
                        y2={tick.y}
                      />
                      <text
                        class="chart-axis-tick"
                        x={scatter().plotLeft - 8}
                        y={tick.y + 4}
                        text-anchor="end"
                      >
                        {tick.label}
                      </text>
                    </g>
                  )}
                </For>

                {/* X axis ticks */}
                <For each={scatter().xTicks}>
                  {(tick) => (
                    <text
                      class="chart-axis-tick"
                      x={tick.x}
                      y={scatter().plotBottom + 18}
                      text-anchor="middle"
                    >
                      {tick.label}
                    </text>
                  )}
                </For>

                {/* Quadrant guide lines */}
                <line
                  class="chart-mid-line"
                  x1={scatter().plotLeft}
                  y1={scatter().midYPos}
                  x2={scatter().plotRight}
                  y2={scatter().midYPos}
                />
                <line
                  class="chart-mid-line"
                  x1={scatter().midXPos}
                  y1={scatter().plotTop}
                  x2={scatter().midXPos}
                  y2={scatter().plotBottom}
                />

                {/* Quadrant labels */}
                <text
                  class="chart-quad-label"
                  x={scatter().plotLeft + 6}
                  y={scatter().plotTop - 8}
                >
                  HIGH-PRECISION SNIPER
                </text>
                <text
                  class="chart-quad-label"
                  x={scatter().plotRight - 4}
                  y={scatter().plotTop - 8}
                  text-anchor="end"
                >
                  STAR REVIEWER (IDEAL)
                </text>
                <text
                  class="chart-quad-label"
                  x={scatter().plotLeft + 6}
                  y={scatter().plotBottom - 8}
                >
                  LOW SIGNAL / QUIET
                </text>
                <text
                  class="chart-quad-label"
                  x={scatter().plotRight - 4}
                  y={scatter().plotBottom - 8}
                  text-anchor="end"
                >
                  THOROUGH BUT NOISY
                </text>

                {/* Axis titles */}
                <text
                  class="chart-axis-title"
                  x={(scatter().plotLeft + scatter().plotRight) / 2}
                  y={scatter().height - 6}
                  text-anchor="middle"
                >
                  Unique bugs found solo per review run (Solo Catch Rate) →
                </text>
                <text
                  class="chart-axis-title"
                  x={14}
                  y={(scatter().plotTop + scatter().plotBottom) / 2}
                  text-anchor="middle"
                  transform={`rotate(-90 14 ${(scatter().plotTop + scatter().plotBottom) / 2})`}
                >
                  Precision (% Adopted) →
                </text>

                {/* Model bubbles */}
                <For each={scatter().points}>
                  {(pt) => (
                    <g class="scatter-point">
                      <title>
                        {`${pt.label} (${pt.lineage.label}) — n=${pt.reviewRuns} reviews, ${(pt.precision * 100).toFixed(1)}% precision, ${pt.soloCatchRate.toFixed(2)} solo catches/run (${pt.soloCatches} solo, ${pt.sharedCatches} shared, ${pt.setAsideCount} set-aside, ${pt.approachPivots} pivots)`}
                      </title>
                      <circle
                        cx={pt.cx}
                        cy={pt.cy}
                        r={pt.r}
                        fill={pt.lineage.colorVar}
                        fill-opacity="0.26"
                        stroke={pt.lineage.colorVar}
                        stroke-width={pt.lowSample ? "1.8" : "2.2"}
                        stroke-dasharray={pt.lowSample ? "3 2" : undefined}
                      />
                      <text
                        class="chart-pt-label"
                        x={pt.labelX}
                        y={pt.labelY}
                        text-anchor={pt.labelAnchor}
                      >
                        {pt.label}
                      </text>
                      <text
                        class="chart-pt-sub"
                        x={pt.labelX}
                        y={pt.labelY + 12}
                        text-anchor={pt.labelAnchor}
                      >
                        {`n=${pt.reviewRuns}${pt.lowSample ? " (low n)" : ""} · ${Math.round(pt.precision * 100)}% prec · ${pt.soloCatchRate.toFixed(2)} solo`}
                      </text>
                    </g>
                  )}
                </For>
              </svg>
            </div>
          </section>
        </Show>

        {/* 2. Model Strength Profile Cards */}
        <Show when={profiles().length > 0}>
          <section class="visual-card" aria-labelledby="profiles-heading">
            <div class="visual-card-head">
              <div>
                <h2 id="profiles-heading">Model strength profiles</h2>
                <p class="field-hint">
                  At-a-glance strengths normalized across your 4 core axes, plus
                  an automatic role label based on observed telemetry.
                </p>
              </div>
            </div>

            <div class="strength-grid">
              <For each={profiles()}>
                {(profile) => (
                  <article class="strength-card">
                    <div class="strength-card-top">
                      <div class="strength-model-name">
                        <span
                          class="lineage-dot"
                          style={{ background: profile.lineage.colorVar }}
                        />
                        <code>{profile.label}</code>
                      </div>
                      <div class="strength-meta">
                        <span>{profile.activityMeta}</span>
                        <Show when={profile.lowSample}>
                          <span class="low-sample-tag">low n</span>
                        </Show>
                      </div>
                    </div>

                    <span class="archetype-pill">{profile.archetype}</span>

                    <div class="strength-bars">
                      <For each={profile.bars}>
                        {(bar) => (
                          <div class="strength-bar-row">
                            <span class="strength-bar-label">{bar.label}</span>
                            <div class="strength-bar-track">
                              <Show when={bar.pct !== null}>
                                <div
                                  class="strength-bar-fill"
                                  style={{
                                    width: `${bar.pct}%`,
                                    background: bar.isDanger
                                      ? "var(--danger)"
                                      : profile.lineage.colorVar,
                                  }}
                                />
                              </Show>
                            </div>
                            <span class="strength-bar-val">{bar.display}</span>
                          </div>
                        )}
                      </For>
                    </div>

                    <p class="strength-summary">{profile.summary}</p>
                  </article>
                )}
              </For>
            </div>
          </section>
        </Show>

        {/* 3. Plan Tournament Lens Heatmap */}
        <Show when={heatmap().rows.length > 0}>
          <section class="visual-card" aria-labelledby="heatmap-heading">
            <div class="visual-card-head">
              <div>
                <h2 id="heatmap-heading">
                  Plan tournament: who wins which lens?
                </h2>
                <p class="field-hint">
                  Shows core plan wins / runs and cherry-picked ideas for each
                  rotated planning lens.
                </p>
              </div>
            </div>

            <div class="table-scroll heatmap-scroll">
              <table class="heatmap-table">
                <thead>
                  <tr>
                    <th>Model</th>
                    <For each={heatmap().columns}>
                      {(col) => <th class="heatmap-col-head">{col.label}</th>}
                    </For>
                  </tr>
                </thead>
                <tbody>
                  <For each={heatmap().rows}>
                    {(row) => (
                      <tr>
                        <td class="heatmap-model-cell">
                          <span
                            class="lineage-dot"
                            style={{ background: row.lineage.colorVar }}
                          />
                          <code>{row.label}</code>
                        </td>
                        <For each={heatmap().columns}>
                          {(col) => {
                            const cell = () => row.cells[col.key];
                            return (
                              <td class="heatmap-td">
                                <Show
                                  when={cell()}
                                  fallback={
                                    <div class="heatmap-cell heatmap-cell-empty">
                                      —
                                    </div>
                                  }
                                >
                                  {(c) => (
                                    <div
                                      class="heatmap-cell"
                                      style={{
                                        "border-color":
                                          c().intensity > 0.12
                                            ? row.lineage.colorVar
                                            : undefined,
                                        background:
                                          c().intensity > 0.12
                                            ? `color-mix(in srgb, ${row.lineage.colorVar} ${Math.round(
                                                10 + c().intensity * 22,
                                              )}%, var(--bg-subtle))`
                                            : undefined,
                                      }}
                                    >
                                      <div class="heatmap-main">
                                        {c().wins} / {c().runs}{" "}
                                        {c().wins === 1 ? "win" : "wins"}
                                      </div>
                                      <div class="heatmap-sub">
                                        {c().cherryPicks > 0
                                          ? `+${c().cherryPicks} cherry-picks`
                                          : `${Math.round(c().winRate * 100)}% win rate`}
                                      </div>
                                    </div>
                                  )}
                                </Show>
                              </td>
                            );
                          }}
                        </For>
                      </tr>
                    )}
                  </For>
                </tbody>
              </table>
            </div>
          </section>
        </Show>

        {/* 4. Plain-English Metric Guide */}
        <details class="metric-guide">
          <summary>How to read these metrics</summary>
          <dl class="metric-guide-list">
            <div>
              <dt>Precision (% Adopted)</dt>
              <dd>
                Signal-to-noise ratio (
                <code>adopted / (adopted + set_aside)</code>). Higher means
                fewer false alarms.
              </dd>
            </div>
            <div>
              <dt>Solo Catch Rate</dt>
              <dd>
                Adopted findings per run that <em>no other reviewer</em> on the
                panel caught. Measures unique bug-hunting value.
              </dd>
            </div>
            <div>
              <dt>Approach Pivots</dt>
              <dd>
                Findings that changed the overall plan or PR verdict, not just a
                local fix.
              </dd>
            </div>
            <div>
              <dt>De-Biased Tournament Score</dt>
              <dd>
                Plan-tournament strength averaged across rotated lenses so a
                harder lens does not unfairly penalize a model.
              </dd>
            </div>
            <div>
              <dt>Speed (% ok / Straggler Rate)</dt>
              <dd>
                Share of parallel runs where this model was <em>not</em> the
                slowest straggler holding up the panel.
              </dd>
            </div>
          </dl>
        </details>
      </Show>
    </div>
  );
}
