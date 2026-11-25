import type {
  CodingModelProfile,
  LensCell,
  ModelScorecard,
} from "./api/client";

export type LineageToneKey = "anthropic" | "openai" | "google" | "other";

export interface LineageTone {
  key: LineageToneKey;
  label: string;
  colorVar: string;
}

export interface ScatterPoint {
  id: string;
  label: string;
  lineage: LineageTone;
  reviewRuns: number;
  soloCatchRate: number;
  precision: number;
  soloCatches: number;
  sharedCatches: number;
  setAsideCount: number;
  approachPivots: number;
  lowSample: boolean;
  cx: number;
  cy: number;
  r: number;
  labelX: number;
  labelY: number;
  labelAnchor: "start" | "middle" | "end";
}

export interface ScatterPlotData {
  width: number;
  height: number;
  plotLeft: number;
  plotRight: number;
  plotTop: number;
  plotBottom: number;
  xMax: number;
  midX: number;
  midY: number;
  midXPos: number;
  midYPos: number;
  xTicks: Array<{ value: number; x: number; label: string }>;
  yTicks: Array<{ value: number; y: number; label: string }>;
  points: ScatterPoint[];
  lineages: LineageTone[];
}

export interface StrengthBar {
  label: string;
  pct: number | null;
  display: string;
  isDanger?: boolean;
}

export interface ModelStrengthProfile {
  id: string;
  label: string;
  lineage: LineageTone;
  isChampion: boolean;
  lowSample: boolean;
  activityMeta: string;
  archetype: string;
  bars: StrengthBar[];
  summary: string;
  totalActivity: number;
}

export interface LensColumn {
  key: string;
  label: string;
}

export interface LensHeatmapRow {
  id: string;
  label: string;
  lineage: LineageTone;
  cells: Record<
    string,
    {
      runs: number;
      wins: number;
      winRate: number;
      cherryPicks: number;
      intensity: number;
    } | null
  >;
}

export interface LensHeatmapData {
  columns: LensColumn[];
  rows: LensHeatmapRow[];
}

export const LOW_SAMPLE_THRESHOLD = 10;

const CANONICAL_LENSES: LensColumn[] = [
  { key: "minimal_diff", label: "Minimal Diff" },
  { key: "clean_architecture", label: "Clean Architecture" },
  { key: "edge_cases", label: "Edge Cases" },
];

export function formatModelKey(model: string, reasoningEffort?: string): string {
  return `${model.trim().toLowerCase()}::${(reasoningEffort ?? "").trim().toLowerCase()}`;
}

export function formatModelName(model: string, reasoningEffort?: string): string {
  return reasoningEffort ? `${model} (${reasoningEffort})` : model;
}

export function lineageTone(lineage: string): LineageTone {
  const normalized = lineage.trim().toLowerCase();
  if (normalized === "anthropic" || normalized === "claude") {
    return {
      key: "anthropic",
      label: "Anthropic",
      colorVar: "var(--chart-anthropic)",
    };
  }
  if (normalized === "openai" || normalized === "gpt") {
    return {
      key: "openai",
      label: "OpenAI",
      colorVar: "var(--chart-openai)",
    };
  }
  if (normalized === "google" || normalized === "gemini") {
    return {
      key: "google",
      label: "Google",
      colorVar: "var(--chart-google)",
    };
  }
  return {
    key: "other",
    label: "Other",
    colorVar: "var(--chart-other)",
  };
}

function clamp(val: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, val));
}

function formatLensLabel(lens: string): string {
  const known = CANONICAL_LENSES.find((c) => c.key === lens);
  if (known) return known.label;
  return lens
    .split(/[_\-\s]+/)
    .filter(Boolean)
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
    .join(" ");
}

function chooseNiceXMax(maxSolo: number): number {
  if (maxSolo <= 1.0) return 1.1;
  return Math.ceil((maxSolo + 0.15) * 4) / 4;
}

export function buildScatterPlot(models: ModelScorecard[]): ScatterPlotData {
  const width = 680;
  const height = 296;
  const plotLeft = 56;
  const plotRight = 656;
  const plotTop = 24;
  const plotBottom = 244;
  const plotW = plotRight - plotLeft;
  const plotH = plotBottom - plotTop;

  const reviewModels = models
    .filter(
      (m) =>
        m.reviewRuns > 0 &&
        m.soloCatches + m.sharedCatches + m.setAsideCount > 0,
    )
    .slice()
    .sort((a, b) => b.reviewRuns - a.reviewRuns);

  const maxSolo = reviewModels.reduce(
    (acc, m) => Math.max(acc, m.soloCatchRate),
    0,
  );
  const xMax = chooseNiceXMax(maxSolo);
  const midX = xMax <= 1.25 ? 0.5 : Math.round((xMax / 2) * 100) / 100;
  const midY = 0.7;

  const toX = (solo: number) => plotLeft + (clamp(solo, 0, xMax) / xMax) * plotW;
  const toY = (prec: number) => plotBottom - clamp(prec, 0, 1) * plotH;

  const midXPos = toX(midX);
  const midYPos = toY(midY);

  const xTickStep = xMax <= 1.25 ? 0.25 : xMax / 4;
  const xTicks: Array<{ value: number; x: number; label: string }> = [];
  for (let i = 0; i <= 4; i++) {
    const value = Math.round(i * xTickStep * 100) / 100;
    if (value > xMax + 0.001) break;
    xTicks.push({
      value,
      x: toX(value),
      label: value.toFixed(2).replace(/\.00$/, ".0"),
    });
  }

  const yTicks = [0, 0.25, 0.5, 0.75, 1].map((value) => ({
    value,
    y: toY(value),
    label: `${Math.round(value * 100)}%`,
  }));

  const rawPoints = reviewModels.map((m) => {
    const r = Math.round(clamp(7 + Math.sqrt(m.reviewRuns) * 1.9, 8, 25));
    const cx = Math.round(toX(m.soloCatchRate));
    const cy = Math.round(toY(m.precision));
    return {
      id: formatModelKey(m.model, m.reasoningEffort),
      label: formatModelName(m.model, m.reasoningEffort),
      lineage: lineageTone(m.lineage),
      reviewRuns: m.reviewRuns,
      soloCatchRate: m.soloCatchRate,
      precision: m.precision,
      soloCatches: m.soloCatches,
      sharedCatches: m.sharedCatches,
      setAsideCount: m.setAsideCount,
      approachPivots: m.approachPivots,
      lowSample: m.reviewRuns < LOW_SAMPLE_THRESHOLD,
      cx,
      cy,
      r,
    };
  });

  // Simple greedy label placement to minimize overlaps between nearby bubbles
  // and top/bottom quadrant header labels.
  const placedBoxes: Array<{ x1: number; y1: number; x2: number; y2: number }> =
    [
      ...rawPoints.map((p) => ({
        x1: p.cx - p.r - 6,
        y1: p.cy - p.r - 6,
        x2: p.cx + p.r + 6,
        y2: p.cy + p.r + 6,
      })),
      // Reserve top and bottom corner quadrant labels
      { x1: plotLeft, y1: plotTop, x2: plotLeft + 170, y2: plotTop + 20 },
      { x1: plotRight - 170, y1: plotTop, x2: plotRight, y2: plotTop + 20 },
      { x1: plotLeft, y1: plotBottom - 20, x2: plotLeft + 160, y2: plotBottom },
      {
        x1: plotRight - 160,
        y1: plotBottom - 20,
        x2: plotRight,
        y2: plotBottom,
      },
    ];

  const points: ScatterPoint[] = rawPoints.map((p, idx) => {
    const estW = Math.max(125, p.label.length * 6.5 + 16);
    const estH = 26;
    const candidates: Array<{
      x: number;
      y: number;
      anchor: "start" | "middle" | "end";
      box: { x1: number; y1: number; x2: number; y2: number };
    }> = [
      // Right of bubble
      {
        x: p.cx + p.r + 7,
        y: p.cy - 2,
        anchor: "start",
        box: {
          x1: p.cx + p.r + 5,
          y1: p.cy - 13,
          x2: p.cx + p.r + 5 + estW,
          y2: p.cy + 13,
        },
      },
      // Below bubble
      {
        x: p.cx,
        y: p.cy + p.r + 14,
        anchor: "middle",
        box: {
          x1: p.cx - estW / 2,
          y1: p.cy + p.r + 3,
          x2: p.cx + estW / 2,
          y2: p.cy + p.r + 3 + estH,
        },
      },
      // Below bubble (right-aligned for points near the right plot edge)
      {
        x: p.cx + p.r,
        y: p.cy + p.r + 14,
        anchor: "end",
        box: {
          x1: p.cx + p.r - estW,
          y1: p.cy + p.r + 3,
          x2: p.cx + p.r,
          y2: p.cy + p.r + 3 + estH,
        },
      },
      // Left of bubble
      {
        x: p.cx - p.r - 7,
        y: p.cy - 2,
        anchor: "end",
        box: {
          x1: p.cx - p.r - 5 - estW,
          y1: p.cy - 13,
          x2: p.cx - p.r - 5,
          y2: p.cy + 13,
        },
      },
      // Above bubble
      {
        x: p.cx,
        y: p.cy - p.r - 16,
        anchor: "middle",
        box: {
          x1: p.cx - estW / 2,
          y1: p.cy - p.r - 28,
          x2: p.cx + estW / 2,
          y2: p.cy - p.r - 2,
        },
      },
    ];

    let best = candidates[0];
    let bestScore = Number.POSITIVE_INFINITY;

    for (const c of candidates) {
      let penalty = 0;
      if (c.box.x1 < 4) penalty += 1000 + (4 - c.box.x1) * 10;
      if (c.box.x2 > width - 4) penalty += 1000 + (c.box.x2 - (width - 4)) * 10;
      if (c.box.y1 < 4) penalty += 1000 + (4 - c.box.y1) * 10;
      if (c.box.y2 > plotBottom + 4) {
        penalty += 1000 + (c.box.y2 - (plotBottom + 4)) * 10;
      }

      for (let j = 0; j < placedBoxes.length; j++) {
        if (j === idx) continue;
        const other = placedBoxes[j];
        const overlapX = Math.max(
          0,
          Math.min(c.box.x2, other.x2) - Math.max(c.box.x1, other.x1),
        );
        const overlapY = Math.max(
          0,
          Math.min(c.box.y2, other.y2) - Math.max(c.box.y1, other.y1),
        );
        if (overlapX > 0 && overlapY > 0) {
          penalty += 25 + (overlapX * overlapY) / 40;
        }
      }

      if (penalty < bestScore) {
        bestScore = penalty;
        best = c;
      }
    }

    // Clamp final label X so it never clips past the left or right SVG canvas edge.
    let finalX = best.x;
    let shiftX = 0;
    if (best.box.x1 < 4) {
      shiftX = 4 - best.box.x1;
    } else if (best.box.x2 > width - 4) {
      shiftX = width - 4 - best.box.x2;
    }
    finalX += shiftX;
    const finalBox = {
      x1: best.box.x1 + shiftX,
      y1: best.box.y1,
      x2: best.box.x2 + shiftX,
      y2: best.box.y2,
    };

    placedBoxes.push(finalBox);
    return {
      ...p,
      labelX: Math.round(finalX),
      labelY: Math.round(best.y),
      labelAnchor: best.anchor,
    };
  });

  const seenLineages = new Map<LineageToneKey, LineageTone>();
  for (const p of points) {
    if (!seenLineages.has(p.lineage.key)) {
      seenLineages.set(p.lineage.key, p.lineage);
    }
  }

  return {
    width,
    height,
    plotLeft,
    plotRight,
    plotTop,
    plotBottom,
    xMax,
    midX,
    midY,
    midXPos,
    midYPos,
    xTicks,
    yTicks,
    points,
    lineages: Array.from(seenLineages.values()),
  };
}

function formatLatency(ms: number): string {
  if (ms >= 60_000) {
    return `${(ms / 60_000).toFixed(1)}m`;
  }
  return `${Math.round(ms / 1000)}s`;
}

function deriveArchetype(
  card: ModelScorecard | undefined,
  coding: CodingModelProfile | undefined,
): string {
  if (!card) {
    if (coding && coding.totalRuns > 0) {
      return coding.firstPassCleanRate >= 0.5
        ? "Clean Solo Coder"
        : "Solo Coding Contributor";
    }
    return "Untested";
  }

  const hasReviews = card.reviewRuns > 0;
  const hasPlans = card.plannerRuns > 0;
  const isStrongPlanner = hasPlans && card.debiasedTournamentScore >= 0.5;

  if (!hasReviews && isStrongPlanner) return "Dedicated Planner";
  if (!hasReviews && hasPlans) return "Plan Contributor";

  if (hasReviews) {
    if (card.reviewRuns < 5 && card.plannerRuns < 5) {
      return "Early Sample";
    }
    if (
      card.precision < 0.4 &&
      card.soloCatches + card.sharedCatches + card.setAsideCount >= 5
    ) {
      return "Noisy / Low Signal";
    }
    if (card.soloCatchRate >= 0.5 && card.precision >= 0.72) {
      return isStrongPlanner ? "Top Planner & Reviewer" : "Star Reviewer";
    }
    if (card.soloCatchRate >= 0.65) {
      return card.criticAdopted >= 5
        ? "Deep Bug Hunter & Critic"
        : "Deep Bug Hunter";
    }
    if (card.precision >= 0.75) {
      if (isStrongPlanner) return "Strong Planner · Clean Reviewer";
      if (
        card.timedRuns > 0 &&
        card.p50DurationMs > 0 &&
        card.p50DurationMs <= 30_000
      ) {
        return "Fast Precision Sniper";
      }
      return "Precision Sniper";
    }
    if (
      card.approachPivots >= 3 &&
      card.approachPivots / card.reviewRuns >= 0.25
    ) {
      return "High-Pivot Reviewer";
    }
    if (isStrongPlanner) return "Strong Planner";
    if (card.reviewRuns < LOW_SAMPLE_THRESHOLD) return "Early Sample";
    return "Balanced Reviewer";
  }

  return "Tracked Model";
}

function buildTakeaway(
  card: ModelScorecard | undefined,
  coding: CodingModelProfile | undefined,
): string {
  const parts: string[] = [];

  if (card && card.reviewRuns > 0) {
    const decided = card.soloCatches + card.sharedCatches + card.setAsideCount;
    if (card.soloCatches > 0) {
      const pivotPart =
        card.approachPivots > 0 ? `, ${card.approachPivots} pivots` : "";
      parts.push(
        `${card.soloCatches} solo ${card.soloCatches === 1 ? "catch" : "catches"}${pivotPart}`,
      );
    } else if (card.sharedCatches > 0) {
      parts.push(`${card.sharedCatches} shared catches`);
    }
    if (decided > 0 && card.setAsideCount > 0) {
      parts.push(`${card.setAsideCount} set-aside`);
    } else if (decided > 0 && card.setAsideCount === 0) {
      parts.push("zero false alarms");
    }
  }

  if (card && card.plannerRuns > 0) {
    if (card.corePlanWins > 0) {
      parts.push(
        `${card.corePlanWins}/${card.plannerRuns} core plan ${card.corePlanWins === 1 ? "win" : "wins"}`,
      );
    } else if (card.cherryPicks > 0) {
      parts.push(`${card.cherryPicks} plan cherry-picks`);
    } else {
      parts.push(`0/${card.plannerRuns} plan wins`);
    }
  }

  if (card && card.timedRuns > 0) {
    if (card.stragglerCount > 0 && card.stragglerRate >= 0.7) {
      parts.push(`slow (${Math.round(card.stragglerRate * 100)}% straggler)`);
    } else if (card.p50DurationMs > 0 && card.p50DurationMs <= 30_000) {
      parts.push(`fast (${formatLatency(card.p50DurationMs)} p50)`);
    }
  }

  if (coding && coding.totalRuns > 0) {
    parts.push(
      `${Math.round(coding.firstPassCleanRate * 100)}% first-pass clean across ${coding.totalRuns} ${coding.totalRuns === 1 ? "PR" : "PRs"}`,
    );
  }

  if (parts.length === 0) {
    return "Not enough completed telemetry yet to characterize strengths.";
  }

  const joined = parts.join(" · ");
  return joined.charAt(0).toUpperCase() + joined.slice(1) + ".";
}

export function buildStrengthProfiles(
  models: ModelScorecard[],
  codingModels: CodingModelProfile[],
): ModelStrengthProfile[] {
  const byKey = new Map<
    string,
    { card?: ModelScorecard; coding?: CodingModelProfile }
  >();

  for (const card of models) {
    if (
      card.reviewRuns === 0 &&
      card.plannerRuns === 0 &&
      card.criticRuns === 0
    ) {
      continue;
    }
    const key = formatModelKey(card.model, card.reasoningEffort);
    byKey.set(key, { card });
  }

  for (const coding of codingModels) {
    if (coding.totalRuns === 0) continue;
    const key = formatModelKey(coding.model, coding.reasoningEffort);
    const existing = byKey.get(key);
    if (existing) {
      existing.coding = coding;
    } else {
      byKey.set(key, { coding });
    }
  }

  const maxSolo = Math.max(
    1.0,
    ...models.map((m) => (m.reviewRuns > 0 ? m.soloCatchRate : 0)),
  );
  const maxPlan = Math.max(
    0.65,
    ...models.map((m) => (m.plannerRuns > 0 ? m.debiasedTournamentScore : 0)),
  );
  const maxP50Ms = Math.max(
    60_000,
    ...models.map((m) => (m.timedRuns > 0 ? m.p50DurationMs : 0)),
  );

  const profiles: ModelStrengthProfile[] = [];

  for (const [id, entry] of byKey.entries()) {
    const { card, coding } = entry;
    const model = card?.model ?? coding!.model;
    const effort = card?.reasoningEffort ?? coding?.reasoningEffort;
    const lineage = lineageTone(card?.lineage ?? coding!.lineage);

    const activityParts: string[] = [];
    if (card && card.reviewRuns > 0) {
      activityParts.push(`${card.reviewRuns} rev`);
    }
    if (card && card.plannerRuns > 0) {
      activityParts.push(`${card.plannerRuns} plan`);
    }
    if (card && card.criticRuns > 0) {
      activityParts.push(`${card.criticRuns} critic`);
    }
    if (coding && coding.totalRuns > 0) {
      activityParts.push(`${coding.totalRuns} PR`);
    }

    const totalActivity =
      (card?.reviewRuns ?? 0) +
      (card?.plannerRuns ?? 0) +
      (card?.criticRuns ?? 0) +
      (coding?.totalRuns ?? 0);

    const decided =
      (card?.soloCatches ?? 0) +
      (card?.sharedCatches ?? 0) +
      (card?.setAsideCount ?? 0);

    const bars: StrengthBar[] = [
      {
        label: "Bug Hunting",
        pct:
          card && card.reviewRuns > 0
            ? Math.round(clamp((card.soloCatchRate / maxSolo) * 100, 4, 100))
            : null,
        display:
          card && card.reviewRuns > 0
            ? `${card.soloCatchRate.toFixed(2)}/r`
            : "—",
      },
      {
        label: "Signal Quality",
        pct: decided > 0 ? Math.round(clamp(card!.precision * 100, 2, 100)) : null,
        display: decided > 0 ? `${(card!.precision * 100).toFixed(0)}%` : "—",
        isDanger: decided >= 5 && card!.precision < 0.4,
      },
      {
        label: "Planning",
        pct:
          card && card.plannerRuns > 0
            ? Math.round(
                clamp((card.debiasedTournamentScore / maxPlan) * 100, 4, 100),
              )
            : null,
        display:
          card && card.plannerRuns > 0
            ? card.debiasedTournamentScore.toFixed(2)
            : "—",
      },
      {
        label: "Speed",
        pct:
          card && card.timedRuns > 0 && card.p50DurationMs > 0
            ? Math.round(
                clamp(
                  (1 - card.p50DurationMs / (maxP50Ms * 1.15)) * 100,
                  10,
                  100,
                ),
              )
            : null,
        display:
          card && card.timedRuns > 0 && card.p50DurationMs > 0
            ? `${formatLatency(card.p50DurationMs)} p50`
            : "—",
      },
    ];

    profiles.push({
      id,
      label: formatModelName(model, effort),
      lineage,
      isChampion: Boolean(card?.isCurrentChampion),
      lowSample: totalActivity < LOW_SAMPLE_THRESHOLD,
      activityMeta: activityParts.join(" · "),
      archetype: deriveArchetype(card, coding),
      bars,
      summary: buildTakeaway(card, coding),
      totalActivity,
    });
  }

  return profiles.sort((a, b) => {
    if (b.totalActivity !== a.totalActivity) {
      return b.totalActivity - a.totalActivity;
    }
    return a.label.localeCompare(b.label);
  });
}

export function buildLensHeatmap(
  lensMatrix: LensCell[],
  models: ModelScorecard[],
): LensHeatmapData {
  if (lensMatrix.length === 0) {
    return { columns: [], rows: [] };
  }

  const lineageByKey = new Map<string, string>();
  for (const m of models) {
    lineageByKey.set(formatModelKey(m.model, m.reasoningEffort), m.lineage);
  }

  const seenLensKeys = new Set(lensMatrix.map((c) => c.lens));
  const columns: LensColumn[] = [];
  for (const canonical of CANONICAL_LENSES) {
    if (seenLensKeys.has(canonical.key)) {
      columns.push(canonical);
      seenLensKeys.delete(canonical.key);
    }
  }
  for (const extra of Array.from(seenLensKeys).sort()) {
    columns.push({ key: extra, label: formatLensLabel(extra) });
  }

  const rowMap = new Map<string, LensHeatmapRow & { score: number }>();

  for (const cell of lensMatrix) {
    const key = formatModelKey(cell.model, cell.reasoningEffort);
    let row = rowMap.get(key);
    if (!row) {
      const cells: LensHeatmapRow["cells"] = {};
      for (const col of columns) {
        cells[col.key] = null;
      }
      row = {
        id: key,
        label: formatModelName(cell.model, cell.reasoningEffort),
        lineage: lineageTone(lineageByKey.get(key) ?? cell.model.split("-")[0] ?? ""),
        cells,
        score: 0,
      };
      rowMap.set(key, row);
    }
    const cherryBoost = Math.min(cell.cherryPicks / Math.max(cell.runs, 1), 3) * 0.12;
    const intensity = clamp(cell.winRate * 0.75 + cherryBoost, 0, 1);
    row.cells[cell.lens] = {
      runs: cell.runs,
      wins: cell.wins,
      winRate: cell.winRate,
      cherryPicks: cell.cherryPicks,
      intensity,
    };
    row.score += cell.wins * 3 + cell.cherryPicks * 0.5 + cell.runs * 0.1;
  }

  const rows = Array.from(rowMap.values())
    .sort((a, b) => b.score - a.score || a.label.localeCompare(b.label))
    .map(({ score: _score, ...rest }) => rest);

  return { columns, rows };
}
