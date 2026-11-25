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
  it("maps known lineages to CSS tokens and unknown lineages to fallback", () => {
    expect(lineageTone("anthropic").colorVar).toBe("var(--chart-anthropic)");
    expect(lineageTone("OpenAI").colorVar).toBe("var(--chart-openai)");
    expect(lineageTone("google").colorVar).toBe("var(--chart-google)");
    expect(lineageTone("local").colorVar).toBe("var(--chart-other)");
  });

  it("scales scatter plot dynamically, marks low-n models, and excludes models without review runs", () => {
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
  });

  it("assigns archetypes, includes coding-only models, and flags low-sample profiles", () => {
    const profiles = buildStrengthProfiles(
      [
        makeScorecard({
          model: "all-rounder",
          reviewRuns: 20,
          precision: 0.78,
          soloCatchRate: 0.65,
          plannerRuns: 10,
          debiasedTournamentScore: 0.6,
        }),
        makeScorecard({
          model: "deep-hunter",
          lineage: "openai",
          reviewRuns: 18,
          precision: 0.55,
          soloCatchRate: 0.85,
          plannerRuns: 2,
          debiasedTournamentScore: 0.2,
          criticAdopted: 1,
        }),
        makeScorecard({
          model: "early-challenger",
          lineage: "google",
          reviewRuns: 3,
          soloCatches: 1,
          sharedCatches: 1,
          setAsideCount: 1,
          approachPivots: 0,
          precision: 0.67,
          soloCatchRate: 0.33,
          plannerRuns: 0,
          criticRuns: 0,
        }),
      ],
      [makeCodingProfile({ model: "solo-coder", totalRuns: 6 })],
    );

    const byLabel = new Map(profiles.map((p) => [p.label, p]));
    expect(byLabel.get("all-rounder (high)")?.archetype).toBe(
      "Top Planner & Reviewer",
    );
    expect(byLabel.get("deep-hunter (high)")?.archetype).toBe(
      "Deep Bug Hunter",
    );
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

