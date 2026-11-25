import { describe, expect, it } from "vitest";
import type {
  CodingModelProfile,
  LensCell,
  ModelScorecard,
} from "./api/client";
import {
  buildLensHeatmap,
  buildScatterPlot,
  buildStrengthProfiles,
  lineageTone,
} from "./visuals";

function makeScorecard(
  overrides: Partial<ModelScorecard> = {},
): ModelScorecard {
  return {
    model: "claude-opus-4.7",
    reasoningEffort: "high",
    lineage: "anthropic",
    isCurrentChampion: true,
    reviewRuns: 15,
    soloCatches: 9,
    sharedCatches: 9,
    setAsideCount: 6,
    openQuestionCount: 0,
    approachPivots: 3,
    soloCatchRate: 0.6,
    precision: 0.75,
    timedRuns: 10,
    p50DurationMs: 25000,
    p90DurationMs: 40000,
    avgDurationMs: 28000,
    stragglerCount: 1,
    stragglerRate: 0.1,
    catchesPerMinute: 2.0,
    failuresCount: 0,
    retriedCount: 0,
    plannerRuns: 12,
    corePlanWins: 6,
    cherryPicks: 8,
    debiasedTournamentScore: 0.55,
    criticRuns: 4,
    criticAdopted: 4,
    criticBloatDropped: 2,
    ...overrides,
  };
}

function makeCodingProfile(
  overrides: Partial<CodingModelProfile> = {},
): CodingModelProfile {
  return {
    model: "claude-sonnet-4.6",
    reasoningEffort: "high",
    lineage: "anthropic",
    totalRuns: 5,
    mergedRuns: 5,
    abandonedRuns: 0,
    firstPassCleanRuns: 4,
    firstPassCleanRate: 0.8,
    zeroTouchRuns: 3,
    zeroTouchRate: 0.6,
    mergedClean24hRuns: 5,
    mergedClean24hRate: 1.0,
    escapedRegressions: 0,
    escapedRegressionRate: 0,
    avgCcrRounds: 1.2,
    totalCcrActionable: 2,
    totalCcrNoise: 1,
    ccrNoiseRatio: 0.33,
    avgHumanThreads: 0.2,
    totalHumanThreads: 1,
    avgRedCiPushes: 0.2,
    avgPromptTurns: 3,
    avgLocChurn: 110,
    avgTestsDelta: 2,
    p50TimeToGreenSec: 90,
    p90TimeToGreenSec: 120,
    p50TimeToMergeSec: 180,
    p90TimeToMergeSec: 240,
    ...overrides,
  };
}

describe("visuals derivations", () => {
  it("maps known lineages to CSS tokens and unknown lineages to Other fallback", () => {
    expect(lineageTone("anthropic").colorVar).toBe("var(--chart-anthropic)");
    expect(lineageTone("OpenAI").colorVar).toBe("var(--chart-openai)");
    expect(lineageTone("google").colorVar).toBe("var(--chart-google)");
    expect(lineageTone("local")).toEqual({
      key: "other",
      label: "Other",
      colorVar: "var(--chart-other)",
    });
  });

  it("scales scatter plot dynamically, marks low-n models, and excludes models without decided findings", () => {
    const plot = buildScatterPlot([
      makeScorecard({
        model: "m-high",
        reasoningEffort: "xhigh",
        lineage: "openai",
        reviewRuns: 20,
        soloCatches: 28,
        soloCatchRate: 1.4,
        precision: 0.8,
      }),
      makeScorecard({
        model: "m-low-n",
        reasoningEffort: "",
        lineage: "google",
        reviewRuns: 4,
        soloCatches: 1,
        soloCatchRate: 0.25,
        precision: 0.5,
      }),
      makeScorecard({
        model: "m-no-decided",
        reviewRuns: 5,
        soloCatches: 0,
        sharedCatches: 0,
        setAsideCount: 0,
        precision: 0,
      }),
      makeScorecard({
        model: "m-plan-only",
        reviewRuns: 0,
        plannerRuns: 6,
      }),
    ]);

    expect(plot.points.map((p) => p.label)).toEqual([
      "m-high (xhigh)",
      "m-low-n",
    ]);
    expect(plot.xMax).toBeGreaterThanOrEqual(1.4);
    expect(plot.points[0]!.lowSample).toBe(false);
    expect(plot.points[1]!.lowSample).toBe(true);
    expect(plot.points[0]!.r).toBeGreaterThan(plot.points[1]!.r);

    // Boundary placement: a long-labeled bubble at the left/top edge stays inside [0, width].
    const edgePlot = buildScatterPlot([
      makeScorecard({
        model: "very-long-left-edge-model-name",
        reasoningEffort: "xhigh",
        reviewRuns: 15,
        soloCatches: 0,
        sharedCatches: 5,
        setAsideCount: 0,
        soloCatchRate: 0,
        precision: 1.0,
      }),
    ]);
    const pt = edgePlot.points[0]!;
    expect(pt.labelX).toBeGreaterThanOrEqual(4);
    expect(pt.labelX).toBeLessThanOrEqual(edgePlot.width - 4);
  });

  it("assigns archetypes, merges case-insensitive keys, computes normalized bars, and handles omitted effort", () => {
    const profiles = buildStrengthProfiles(
      [
        makeScorecard({
          model: "All-Rounder",
          reasoningEffort: "HIGH",
          reviewRuns: 20,
          precision: 0.78,
          soloCatchRate: 0.65,
          plannerRuns: 10,
          debiasedTournamentScore: 0.6,
          p50DurationMs: 25_000,
          stragglerRate: 0.2,
        }),
        makeScorecard({
          model: "deep-hunter",
          reasoningEffort: undefined,
          lineage: "openai",
          reviewRuns: 18,
          soloCatches: 2,
          sharedCatches: 0,
          setAsideCount: 6,
          precision: 0.25,
          soloCatchRate: 0.85,
          plannerRuns: 0,
          debiasedTournamentScore: 0,
          criticAdopted: 1,
          timedRuns: 3,
          p50DurationMs: 0,
          stragglerCount: 0,
          stragglerRate: 0,
        }),
        makeScorecard({
          model: "early-challenger",
          lineage: "google",
          reviewRuns: 2,
          soloCatches: 2,
          sharedCatches: 0,
          setAsideCount: 0,
          approachPivots: 0,
          precision: 1.0,
          soloCatchRate: 1.0,
          plannerRuns: 0,
          criticRuns: 0,
        }),
      ],
      [
        makeCodingProfile({
          model: "all-rounder",
          reasoningEffort: "high",
          totalRuns: 4,
          firstPassCleanRate: 0.75,
        }),
        makeCodingProfile({ model: "solo-coder", totalRuns: 6 }),
      ],
    );

    const byLabel = new Map(profiles.map((p) => [p.label, p]));
    const allRounder = byLabel.get("All-Rounder (HIGH)");
    expect(allRounder?.archetype).toBe("Top Planner & Reviewer");
    expect(allRounder?.activityMeta).toContain("4 PR");
    expect(allRounder?.summary).toContain("75% first-pass clean across 4 PRs");
    expect(allRounder?.summary).toContain("fast (25s p50)");
    expect(allRounder?.bars).toEqual([
      { label: "Bug Hunting", pct: 65, display: "0.65/r" },
      { label: "Signal Quality", pct: 78, display: "78%", isDanger: false },
      { label: "Planning", pct: 92, display: "0.60" },
      { label: "Speed", pct: 64, display: "25s p50" },
    ]);

    const noisy = byLabel.get("deep-hunter");
    expect(noisy?.archetype).toBe("Noisy / Low Signal");
    expect(noisy?.bars[1]?.isDanger).toBe(true);
    expect(noisy?.bars[2]).toEqual({
      label: "Planning",
      pct: null,
      display: "—",
    });
    // Zero p50 duration must not render a fake fast Speed bar.
    expect(noisy?.bars[3]).toEqual({
      label: "Speed",
      pct: null,
      display: "—",
    });

    // A 2-run model with 100% precision must stay Early Sample, not Star Reviewer.
    expect(byLabel.get("early-challenger (high)")?.archetype).toBe(
      "Early Sample",
    );
    expect(byLabel.get("early-challenger (high)")?.lowSample).toBe(true);
    expect(byLabel.get("solo-coder (high)")?.archetype).toBe(
      "Clean Solo Coder",
    );
  });

  it("pivots plan tournament lens cells in canonical order and preserves missing cells as null", () => {
    const cells: LensCell[] = [
      {
        model: "gpt-5.4",
        reasoningEffort: "xhigh",
        lens: "minimal_diff",
        runs: 6,
        wins: 1,
        winRate: 1 / 6,
        cherryPicks: 2,
      },
      {
        model: "claude-opus-4.7",
        reasoningEffort: "high",
        lens: "clean_architecture",
        runs: 5,
        wins: 4,
        winRate: 0.8,
        cherryPicks: 6,
      },
      {
        model: "claude-opus-4.7",
        reasoningEffort: "high",
        lens: "edge_cases",
        runs: 4,
        wins: 3,
        winRate: 0.75,
        cherryPicks: 4,
      },
    ];

    const heatmap = buildLensHeatmap(cells, [
      makeScorecard({ model: "claude-opus-4.7", reasoningEffort: "high" }),
      makeScorecard({
        model: "gpt-5.4",
        reasoningEffort: "xhigh",
        lineage: "openai",
      }),
    ]);

    expect(heatmap.columns.map((c) => c.key)).toEqual([
      "minimal_diff",
      "clean_architecture",
      "edge_cases",
    ]);
    expect(heatmap.rows.map((r) => r.label)).toEqual([
      "claude-opus-4.7 (high)",
      "gpt-5.4 (xhigh)",
    ]);
    expect(heatmap.rows[0]!.cells["clean_architecture"]?.wins).toBe(4);
    expect(heatmap.rows[0]!.cells["minimal_diff"]).toBeNull();
  });
});

