import { describe, expect, it } from "vitest";
import type { ProviderStatus, SourceProvenance } from "./api";
import { freshnessMessage } from "./freshness";

const missing: SourceProvenance = {
  providerId: "tracker:ops",
  tracker: "ops",
  state: "missing",
  refreshState: "pending"
};

function provider(overrides: Partial<ProviderStatus> = {}): ProviderStatus {
  return {
    id: "tracker:ops",
    displayName: "OPS",
    kind: "tracker",
    state: "available",
    consecutiveFailures: 0,
    minimumIntervalMs: 2500,
    safeMinimumIntervalMs: 2500,
    backgroundMinimumIntervalMs: 7000,
    safeBackgroundMinimumIntervalMs: 7000,
    maxConcurrency: 1,
    safeMaxConcurrency: 1,
    queued: { interactive: 0, download: 0, manual: 0, scheduled: 0, background: 0 },
    canPause: true,
    canResume: false,
    ...overrides
  };
}

describe("freshnessMessage", () => {
  it("describes a cold catalogue as loading without claiming an outage", () => {
    expect(freshnessMessage(missing, provider())).toContain("Loading releases from OPS");
    expect(freshnessMessage(missing, provider())).not.toContain("unavailable");
  });

  it("reports provider rate limiting and retains the cached timestamp", () => {
    const message = freshnessMessage(
      {
        ...missing,
        state: "stale",
        fetchedAt: "2026-08-02T14:00:00Z"
      },
      provider({
        state: "cooldown",
        reasonCode: "rate_limited",
        retryAt: "2026-08-02T14:15:00Z"
      })
    );
    expect(message).toContain("temporarily rate-limited");
    expect(message).toContain("Showing the snapshot from");
  });

  it("distinguishes an unresolved artist source from provider failure", () => {
    expect(freshnessMessage({
      ...missing,
      refreshState: undefined,
      errorCode: "artist_source_unresolved"
    })).toContain("catalogue link for this artist is not known yet");
  });
});
