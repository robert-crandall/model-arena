import { render } from "@solidjs/web";
import { flush } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";
import type {
  ApiToken,
  CodingLeaderboard,
  Leaderboard,
  User,
} from "./lib/api/client";



function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

async function waitFor(assertion: () => void, timeoutMs = 1500): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  let lastError: unknown;
  while (Date.now() < deadline) {
    flush();
    try {
      assertion();
      return;
    } catch (err) {
      lastError = err;
      await new Promise((resolve) => setTimeout(resolve, 10));
    }
  }
  throw lastError;
}

function setInputValue(input: HTMLInputElement, value: string) {
  input.value = value;
  input.dispatchEvent(new Event("input", { bubbles: true }));
  flush();
}

describe("Model Arena SPA foundation", () => {
  let currentUser: User | null;
  let registrationOpen: boolean;
  let tokens: ApiToken[];
  let tokenLoads: number;
  let failAppBootstrap: boolean;
  let failLoginNetwork: boolean;
  let failTokenLoad: boolean;
  let pendingTokenLoad: ((res: Response) => void) | null;
  let dropCreatedTokenResponse: boolean;
  let failLogoutStatus: boolean;
  let leaderboardData: Leaderboard;
  let codingLeaderboardData: CodingLeaderboard;
  let dispose: (() => void) | undefined;

  beforeEach(() => {
    currentUser = null;
    registrationOpen = true;
    tokens = [];
    tokenLoads = 0;
    failAppBootstrap = false;
    failLoginNetwork = false;
    failTokenLoad = false;
    pendingTokenLoad = null;
    dropCreatedTokenResponse = false;
    failLogoutStatus = false;
    leaderboardData = {
      windowDays: 90,
      totalRuns: 0,
      models: [],
      lensMatrix: [],
      hostBiasMatrix: [],
      trials: [],
      roster: [],
      recentRuns: [],
    };
    codingLeaderboardData = {
      windowDays: 90,
      totalRuns: 0,
      models: [],
      slices: [],
      recentRuns: [],
    };



    let currentTheme: "system" | "light" | "dark" = "system";
    window.appTheme = {
      choices: [
        { value: "system", label: "System" },
        { value: "light", label: "Light" },
        { value: "dark", label: "Dark" },
      ],
      get: () => currentTheme,
      set: (val) => {
        currentTheme = val;
        if (val === "system") {
          document.documentElement.removeAttribute("data-theme");
        } else {
          document.documentElement.dataset.theme = val;
        }
      },
    };

    vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const req = input instanceof Request ? input : new Request(String(input), init);
      const url = new URL(req.url);
      const method = req.method.toUpperCase();

      if (url.pathname === "/api/app" && method === "GET") {
        if (failAppBootstrap) {
          throw new TypeError("Failed to fetch");
        }
        return jsonResponse({ registrationOpen });
      }

      if (url.pathname === "/api/auth/me" && method === "GET") {
        if (!currentUser) {
          return jsonResponse({ error: "authentication required" }, 401);
        }
        return jsonResponse(currentUser);
      }

      if (url.pathname === "/api/auth/login" && method === "POST") {
        if (failLoginNetwork) {
          throw new TypeError("Failed to fetch");
        }
        const body = (await req.json()) as { email: string; password: string };
        if (body.password !== "correct-horse-battery") {
          return jsonResponse({ error: "invalid email or password" }, 401);
        }
        currentUser = {
          id: "00000000-0000-0000-0000-000000000001",
          email: body.email.toLowerCase(),
          name: "Leo",
          createdAt: "2026-04-20T00:00:00Z",
        };
        return jsonResponse(currentUser);
      }

      if (url.pathname === "/api/auth/register" && method === "POST") {
        const body = (await req.json()) as {
          email: string;
          password: string;
          name?: string | null;
        };
        currentUser = {
          id: "00000000-0000-0000-0000-000000000002",
          email: body.email.toLowerCase(),
          name: body.name ?? "",
          createdAt: "2026-04-20T00:00:00Z",
        };
        return jsonResponse(currentUser, 201);
      }

      if (url.pathname === "/api/auth/logout" && method === "POST") {
        if (failLogoutStatus) {
          return jsonResponse({ error: "session store unavailable" }, 500);
        }
        currentUser = null;
        return new Response(null, { status: 204 });
      }

      if (url.pathname === "/api/tokens" && method === "GET") {
        tokenLoads += 1;
        if (pendingTokenLoad) {
          const resolver = pendingTokenLoad;
          pendingTokenLoad = null;
          return new Promise<Response>((resolve) => {
            pendingTokenLoad = resolve;
            void resolver;
          });
        }
        if (failTokenLoad) {
          return jsonResponse({ error: "database unavailable" }, 500);
        }
        return jsonResponse(tokens);
      }

      if (url.pathname === "/api/tokens" && method === "POST") {
        const body = (await req.json()) as { name: string };
        const created = {
          id: "00000000-0000-0000-0000-000000000099",
          name: body.name,
          last4: "a1b2",
          createdAt: "2026-04-20T00:00:00Z",
          token: "pat_00000000-0000-0000-0000-000000000099_secret_a1b2",
        };
        tokens = [
          {
            id: created.id,
            name: created.name,
            last4: created.last4,
            createdAt: created.createdAt,
            lastUsedAt: null,
          },
          ...tokens,
        ];
        if (dropCreatedTokenResponse) {
          throw new TypeError("Failed to fetch");
        }
        return jsonResponse(created, 201);
      }

      if (url.pathname.startsWith("/api/tokens/") && method === "DELETE") {
        const id = url.pathname.slice("/api/tokens/".length);
        tokens = tokens.filter((item) => item.id !== id);
        return new Response(null, { status: 204 });
      }

      if (url.pathname === "/api/leaderboard" && method === "GET") {
        return jsonResponse(leaderboardData);
      }

      if (url.pathname === "/api/coding-leaderboard" && method === "GET") {
        return jsonResponse(codingLeaderboardData);
      }

      return jsonResponse({ error: `unhandled ${method} ${url.pathname}` }, 404);

    });
  });

  afterEach(() => {
    dispose?.();
    dispose = undefined;
    vi.restoreAllMocks();
  });

  it("registers a household account and lands in the shared workspace", async () => {
    const host = document.createElement("div");
    document.body.appendChild(host);
    dispose = render(() => <App />, host);

    await waitFor(() => {
      expect(host.querySelector("h1")?.textContent).toBe("Sign in");
    });

    const toggle = Array.from(host.querySelectorAll("button")).find((btn) =>
      btn.textContent?.includes("Create an account"),
    );
    expect(toggle).toBeDefined();
    toggle!.click();
    flush();

    expect(host.querySelector("h1")?.textContent).toBe("Create household account");

    setInputValue(host.querySelector("#auth-name") as HTMLInputElement, "Dad");
    setInputValue(
      host.querySelector("#auth-email") as HTMLInputElement,
      "dad@example.com",
    );
    setInputValue(
      host.querySelector("#auth-password") as HTMLInputElement,
      "very-strong-passphrase",
    );

    host
      .querySelector("form")!
      .dispatchEvent(new SubmitEvent("submit", { bubbles: true, cancelable: true }));

    await waitFor(() => {
      expect(host.querySelector("h1")?.textContent).toBe(
        "Ready for Model Arena",
      );
      expect(host.textContent).toContain("Signed in as Dad");
    });
  });

  it("shows inline alert when sign-in credentials are rejected", async () => {
    const host = document.createElement("div");
    document.body.appendChild(host);
    dispose = render(() => <App />, host);

    await waitFor(() => {
      expect(host.querySelector("h1")?.textContent).toBe("Sign in");
    });

    setInputValue(
      host.querySelector("#auth-email") as HTMLInputElement,
      "leo@example.com",
    );
    setInputValue(
      host.querySelector("#auth-password") as HTMLInputElement,
      "wrong-password",
    );

    host
      .querySelector("form")!
      .dispatchEvent(new SubmitEvent("submit", { bubbles: true, cancelable: true }));

    await waitFor(() => {
      const alert = host.querySelector('[role="alert"]');
      expect(alert?.textContent).toBe("invalid email or password");
    });
  });

  it("keeps API token details collapsed until opened, supports mint/revoke, theme switch, and sign-out", async () => {
    currentUser = {
      id: "00000000-0000-0000-0000-000000000001",
      email: "leo@example.com",
      name: "Leo",
      createdAt: "2026-04-20T00:00:00Z",
    };

    const host = document.createElement("div");
    document.body.appendChild(host);
    dispose = render(() => <App />, host);

    await waitFor(() => {
      expect(host.querySelector("h1")?.textContent).toBe(
        "Ready for Model Arena",
      );
    });

    // Progressive disclosure: tokens are not fetched until the details panel opens.
    expect(tokenLoads).toBe(0);

    const themeSelect = host.querySelector(
      'select[aria-label="Color theme"]',
    ) as HTMLSelectElement;
    themeSelect.value = "dark";
    themeSelect.dispatchEvent(new Event("change", { bubbles: true }));
    flush();
    expect(document.documentElement.dataset.theme).toBe("dark");

    const details = host.querySelector("details") as HTMLDetailsElement;
    details.open = true;
    details.dispatchEvent(new Event("toggle"));

    await waitFor(() => {
      expect(tokenLoads).toBe(1);
    });

    setInputValue(
      host.querySelector("#new-token-name") as HTMLInputElement,
      "model-arena-rust-mcp",
    );
    details
      .querySelector("form")!
      .dispatchEvent(new SubmitEvent("submit", { bubbles: true, cancelable: true }));

    await waitFor(() => {
      expect(host.textContent).toContain(
        "pat_00000000-0000-0000-0000-000000000099_secret_a1b2",
      );
      expect(host.querySelector(".token-name")?.textContent).toBe(
        "model-arena-rust-mcp",
      );
    });

    const revokeBtn = Array.from(host.querySelectorAll("button")).find(
      (btn) => btn.textContent === "Revoke",
    );
    revokeBtn!.click();

    await waitFor(() => {
      expect(host.querySelector(".token-name")).toBeNull();
    });

    const signOutBtn = Array.from(host.querySelectorAll("button")).find(
      (btn) => btn.textContent === "Sign out",
    );
    signOutBtn!.click();

    await waitFor(() => {
      expect(host.querySelector("h1")?.textContent).toBe("Sign in");
    });
  });

  it("handles network failures on sign-in, token load, and sign-out without corrupting UI state", async () => {
    failLoginNetwork = true;
    failTokenLoad = true;
    failLogoutStatus = true;

    const host = document.createElement("div");
    document.body.appendChild(host);
    dispose = render(() => <App />, host);

    await waitFor(() => {
      expect(host.querySelector("h1")?.textContent).toBe("Sign in");
    });

    setInputValue(
      host.querySelector("#auth-email") as HTMLInputElement,
      "leo@example.com",
    );
    setInputValue(
      host.querySelector("#auth-password") as HTMLInputElement,
      "correct-horse-battery",
    );
    host
      .querySelector("form")!
      .dispatchEvent(new SubmitEvent("submit", { bubbles: true, cancelable: true }));

    await waitFor(() => {
      expect(host.querySelector('[role="alert"]')?.textContent).toContain(
        "Could not reach the server",
      );
      expect(
        (host.querySelector('button[type="submit"]') as HTMLButtonElement).disabled,
      ).toBe(false);
    });

    failLoginNetwork = false;
    host
      .querySelector("form")!
      .dispatchEvent(new SubmitEvent("submit", { bubbles: true, cancelable: true }));

    await waitFor(() => {
      expect(host.querySelector("h1")?.textContent).toBe(
        "Ready for Model Arena",
      );
    });

    const details = host.querySelector("details") as HTMLDetailsElement;
    details.open = true;
    details.dispatchEvent(new Event("toggle"));

    await waitFor(() => {
      expect(host.textContent).toContain("database unavailable");
    });
    expect(host.textContent).not.toContain(
      "No personal access tokens minted for this account yet.",
    );

    // Recover from a committed mint whose response dropped: reopening refreshes tokens.
    failTokenLoad = false;
    details.open = false;
    details.dispatchEvent(new Event("toggle"));
    details.open = true;
    details.dispatchEvent(new Event("toggle"));
    await waitFor(() => {
      expect(host.textContent).toContain(
        "No personal access tokens minted for this account yet.",
      );
    });

    dropCreatedTokenResponse = true;
    setInputValue(
      host.querySelector("#new-token-name") as HTMLInputElement,
      "dropped-response-token",
    );
    details
      .querySelector("form")!
      .dispatchEvent(new SubmitEvent("submit", { bubbles: true, cancelable: true }));
    await waitFor(() => {
      expect(host.textContent).toContain("Reopen this panel to check existing tokens");
    });

    details.open = false;
    details.dispatchEvent(new Event("toggle"));
    details.open = true;
    details.dispatchEvent(new Event("toggle"));
    await waitFor(() => {
      expect(host.querySelector(".token-name")?.textContent).toBe(
        "dropped-response-token",
      );
    });

    const signOutBtn = Array.from(host.querySelectorAll("button")).find(
      (btn) => btn.textContent === "Sign out",
    );
    signOutBtn!.click();

    await waitFor(() => {
      expect(host.textContent).toContain("session store unavailable");
      expect(host.querySelector("h1")?.textContent).toBe(
        "Ready for Model Arena",
      );
    });
  });

  it("preserves authenticated user if /api/app fails and ignores stale token-list responses", async () => {
    failAppBootstrap = true;
    currentUser = {
      id: "00000000-0000-0000-0000-000000000001",
      email: "leo@example.com",
      name: "Leo",
      createdAt: "2026-04-20T00:00:00Z",
    };
    const initialToken: ApiToken = {
      id: "00000000-0000-0000-0000-000000000077",
      name: "old-token",
      last4: "7777",
      createdAt: "2026-04-20T00:00:00Z",
      lastUsedAt: null,
    };
    tokens = [initialToken];

    const host = document.createElement("div");
    document.body.appendChild(host);
    dispose = render(() => <App />, host);

    await waitFor(() => {
      expect(host.querySelector("h1")?.textContent).toBe(
        "Ready for Model Arena",
      );
      expect(host.textContent).toContain("Signed in as Leo");
    });

    const details = host.querySelector("details") as HTMLDetailsElement;
    details.open = true;
    details.dispatchEvent(new Event("toggle"));
    await waitFor(() => {
      expect(host.querySelector(".token-name")?.textContent).toBe("old-token");
    });

    // Start a slow reopen refresh, then revoke before that stale GET resolves.
    pendingTokenLoad = () => {};
    details.open = false;
    details.dispatchEvent(new Event("toggle"));
    details.open = true;
    details.dispatchEvent(new Event("toggle"));

    const resolveStaleGet = pendingTokenLoad!;
    pendingTokenLoad = null;

    const revokeBtn = Array.from(host.querySelectorAll("button")).find(
      (btn) => btn.textContent === "Revoke",
    );
    revokeBtn!.click();
    await waitFor(() => {
      expect(host.querySelector(".token-name")).toBeNull();
    });

    resolveStaleGet(jsonResponse([initialToken]));
    await new Promise((resolve) => setTimeout(resolve, 30));
    flush();
    expect(host.querySelector(".token-name")).toBeNull();

    // Seed another old token, hold the post-mint GET open, and revoke the old
    // token before that GET settles; the newly minted token must stay visible.
    tokens = [initialToken];
    details.open = false;
    details.dispatchEvent(new Event("toggle"));
    details.open = true;
    details.dispatchEvent(new Event("toggle"));
    await waitFor(() => {
      expect(host.querySelector(".token-name")?.textContent).toBe("old-token");
    });

    pendingTokenLoad = () => {};
    setInputValue(
      host.querySelector("#new-token-name") as HTMLInputElement,
      "fresh-token",
    );
    details
      .querySelector("form")!
      .dispatchEvent(new SubmitEvent("submit", { bubbles: true, cancelable: true }));

    await waitFor(() => {
      const names = Array.from(host.querySelectorAll(".token-name")).map(
        (el) => el.textContent,
      );
      expect(names).toEqual(["fresh-token", "old-token"]);
    });

    const resolvePostMintGet = pendingTokenLoad!;
    pendingTokenLoad = null;

    const oldRow = Array.from(host.querySelectorAll(".token-item")).find((el) =>
      el.textContent?.includes("old-token"),
    )!;
    (oldRow.querySelector("button") as HTMLButtonElement).click();

    await waitFor(() => {
      const names = Array.from(host.querySelectorAll(".token-name")).map(
        (el) => el.textContent,
      );
      expect(names).toEqual(["fresh-token"]);
    });

    resolvePostMintGet(jsonResponse(tokens));
    await new Promise((resolve) => setTimeout(resolve, 30));
    flush();
    expect(
      Array.from(host.querySelectorAll(".token-name")).map((el) => el.textContent),
    ).toEqual(["fresh-token"]);
  });

  it("renders read-only review, tournament, coding, and trial telemetry without model-promotion buttons", async () => {
    currentUser = {
      id: "00000000-0000-0000-0000-000000000001",
      email: "leo@example.com",
      name: "Leo",
      createdAt: "2026-04-20T00:00:00Z",
    };
    const sampleCard = {
      model: "claude-opus-4.7",
      reasoningEffort: "high",
      lineage: "claude",
      isCurrentChampion: true,
      activeChallengerTrialId: null,
      reviewRuns: 3,
      soloCatches: 3,
      sharedCatches: 2,
      setAsideCount: 1,
      openQuestionCount: 0,
      approachPivots: 1,
      precision: 0.8333,
      soloCatchRate: 1.0,
      plannerRuns: 1,
      corePlanWins: 1,
      cherryPicks: 2,
      debiasedTournamentScore: 1.0,
      criticRuns: 1,
      criticAdopted: 1,
      criticBloatDropped: 0,
      failuresCount: 0,
      retriedCount: 0,
      timedRuns: 4,
      p50DurationMs: 28000,
      p90DurationMs: 42000,
      avgDurationMs: 31000,
      stragglerCount: 0,
      stragglerRate: 0.1,
      catchesPerMinute: 2.15,
    };
    leaderboardData = {
      windowDays: 90,
      totalRuns: 4,
      models: [sampleCard],
      lensMatrix: [
        {
          model: "claude-opus-4.7",
          reasoningEffort: "high",
          lens: "minimalist",
          runs: 1,
          wins: 1,
          winRate: 1.0,
          cherryPicks: 2,
        },
      ],
      hostBiasMatrix: [
        {
          hostLineage: "gemini",
          hostModel: "gemini-3.1-pro",
          reviewerLineage: "claude",
          reviewerModel: "claude-opus-4.7",
          candidatesEvaluated: 6,
          adoptedCount: 5,
          adoptionRate: 0.8333,
        },
      ],
      trials: [
        {
          trial: {
            id: 7,
            skill: "adversarial-review",
            lineage: "gpt",
            championModel: "gpt-5.4",
            championReasoningEffort: "high",
            challengerModel: "gpt-5.4",
            challengerReasoningEffort: "xhigh",
            swapPercent: 30,
            targetRuns: 10,
            completedRuns: 10,
            allowPrDiff: true,
            status: "active",
            createdAt: "2026-04-20T00:00:00Z",
            decidedAt: null,
          },
          champion: {
            ...sampleCard,
            model: "gpt-5.4",
            reasoningEffort: "high",
            lineage: "gpt",
            precision: 0.8,
            p50DurationMs: 45000,
          },
          challenger: {
            ...sampleCard,
            model: "gpt-5.4",
            reasoningEffort: "xhigh",
            lineage: "gpt",
            precision: 0.9,
            p50DurationMs: 22000,
          },
          p50DeltaMs: -23000,
          speedupRatio: 2.045,
          verdictHint: "promote_challenger",
          verdictNotes:
            "Challenger leads quality (+0.220 score) and is faster (p50 22000ms vs 45000ms).",
        },
      ],
      roster: [
        {
          skill: "adversarial-review",
          lineage: "claude",
          championModel: "claude-opus-4.7",
          championReasoningEffort: "high",
          backupModel: "claude-sonnet-4.6",
          backupReasoningEffort: "",
          sortOrder: 0,
          enabled: true,
          updatedAt: "2026-04-20T00:00:00Z",
        },
      ],
      recentRuns: [
        {
          id: 10,
          runKey: "adv-run-10",
          skill: "adversarial-review",
          occurredAt: "2026-04-20T00:00:00Z",
          repo: "robert-crandall/model-arena-rust",
          machine: "mbp",
          harness: "copilot-cli",
          hostModel: "gemini-3.1-pro",
          hostReasoningEffort: "",
          artifactKind: "pr_diff",
          artifactRef: "PR #5",
          approachVerdict: "sound",
          winningRole: "",
          legs: [],
          candidates: [],
        },
      ],
    };
    codingLeaderboardData = {
      windowDays: 90,
      totalRuns: 2,
      models: [
        {
          model: "claude-opus-4.7",
          reasoningEffort: "high",
          lineage: "claude",
          totalRuns: 2,
          mergedRuns: 2,
          abandonedRuns: 0,
          firstPassCleanRuns: 1,
          firstPassCleanRate: 0.5,
          zeroTouchRuns: 1,
          zeroTouchRate: 0.5,
          mergedClean24hRuns: 2,
          mergedClean24hRate: 1.0,
          escapedRegressions: 0,
          escapedRegressionRate: 0,
          avgCcrRounds: 1.5,
          totalCcrActionable: 1,
          totalCcrNoise: 0,
          ccrNoiseRatio: 0,
          avgHumanThreads: 0,
          totalHumanThreads: 0,
          avgRedCiPushes: 0.5,
          avgPromptTurns: 3,
          avgLocChurn: 120,
          avgTestsDelta: 2,
          p50TimeToGreenSec: 90,
          p90TimeToGreenSec: 120,
          p50TimeToMergeSec: 180,
          p90TimeToMergeSec: 240,
        },
      ],
      slices: [],
      recentRuns: [],
    };

    const host = document.createElement("div");
    document.body.appendChild(host);
    dispose = render(() => <App />, host);

    await waitFor(() => {
      expect(host.textContent).toContain("claude-opus-4.7 (high)");
      expect(host.textContent).toContain("28.0s / 42.0s");
    });

    // Switch through tabs and verify read-only views
    const tabs = Array.from(
      host.querySelectorAll<HTMLButtonElement>(".tab-btn"),
    );
    const codingTab = tabs.find((btn) =>
      btn.textContent?.includes("Coding & PRs"),
    )!;
    codingTab.click();
    flush();
    expect(host.textContent).toContain("Single-model coding & PR profiles");
    expect(host.textContent).toContain("claude-opus-4.7 (high)");
    expect(host.textContent).toContain("3.0m / 4.0m");

    const trialsTab = tabs.find((btn) =>
      btn.textContent?.includes("Challenger Trials"),
    )!;
    trialsTab.click();
    flush();
    expect(host.textContent).toContain("gpt-5.4 (high)");
    expect(host.textContent).toContain("gpt-5.4 (xhigh)");
    expect(host.textContent).toContain("claude-sonnet-4.6");
    expect(host.textContent).not.toContain("claude-sonnet-4.6 ()");
    expect(host.textContent).toContain(
      "Challenger leads quality (+0.220 score)",
    );

    // Ensure there are no buttons to promote or retire trials in the UI
    const allButtons = Array.from(host.querySelectorAll("button")).map((b) =>
      (b.textContent ?? "").toLowerCase(),
    );
    expect(allButtons.some((label) => label.includes("promote"))).toBe(false);
    expect(allButtons.some((label) => label.includes("retire"))).toBe(false);
  });

});

