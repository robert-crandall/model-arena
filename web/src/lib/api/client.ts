import createClient from "openapi-fetch";
import type { components, paths } from "./schema";

const baseUrl = typeof window !== "undefined" ? window.location.origin : "";

/**
 * Shared typed API client generated from docs/openapi.json.
 * Same-origin session cookies accompany browser requests automatically.
 */
export const api = createClient<paths>({
  baseUrl,
  fetch: (request) => globalThis.fetch(request),
});

export type User = components["schemas"]["User"];
export type AppInfo = components["schemas"]["AppInfo"];
export type ApiToken = components["schemas"]["ApiToken"];
export type CreatedApiToken = components["schemas"]["CreatedApiToken"];
export type LoginRequest = components["schemas"]["LoginRequest"];
export type RegisterRequest = components["schemas"]["RegisterRequest"];
export type Leaderboard = components["schemas"]["Leaderboard"];
export type ModelScorecard = components["schemas"]["ModelScorecard"];
export type LensCell = components["schemas"]["LensCell"];
export type HostBiasCell = components["schemas"]["HostBiasCell"];
export type TrialComparison = components["schemas"]["TrialComparison"];
export type RosterEntry = components["schemas"]["RosterEntry"];
export type RecentRunView = components["schemas"]["RecentRunView"];
export type CodingLeaderboard = components["schemas"]["CodingLeaderboard"];
export type CodingModelProfile = components["schemas"]["CodingModelProfile"];
export type CodingSliceCell = components["schemas"]["CodingSliceCell"];
export type CodingRunRecord = components["schemas"]["CodingRunRecord"];


