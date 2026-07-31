import { describe, expect, it } from "vitest";
import type {
  ProviderPolicyOverride,
  QualityPreference,
  TrackerPreference,
  VariantSortCriterion
} from "./api";
import { buildPreferencePayload } from "./preferencePayload";

function reactiveArray<T>(values: T[]): T[] {
  return new Proxy(values, {});
}

describe("buildPreferencePayload", () => {
  it("converts deeply proxied preference state into cloneable request data", () => {
    const qualityTiers = reactiveArray([
      reactiveArray<QualityPreference>(["lossless"]),
      reactiveArray<QualityPreference>(["320"])
    ]);
    const mediaTiers = reactiveArray([
      reactiveArray(["WEB", "CD"]),
      reactiveArray(["Vinyl"])
    ]);
    const variantSortOrder = reactiveArray<VariantSortCriterion>([
      "quality",
      "tracker",
      "media",
      "edition"
    ]);
    const trackerOrder = reactiveArray(["ops"]);
    const trackerPolicies = reactiveArray<TrackerPreference>([
      new Proxy(
        {
          tracker: "ops",
          mode: "freeleech_or_token",
          autoUseTokens: true,
          downloadProfile: "ops",
          autoTokenLimit: 500
        },
        {}
      )
    ]);
    const apiPolicies = new Proxy<Record<string, ProviderPolicyOverride>>(
      { ops: new Proxy({ minimumIntervalMs: 3000 }, {}) },
      {}
    );

    expect(() => structuredClone(qualityTiers)).toThrow();

    const payload = buildPreferencePayload({
      qualityTiers,
      qualityCutoffIndex: 1,
      mediaTiers,
      mediaCutoffIndex: 1,
      variantSortOrder,
      trackerOrder,
      trackerPolicies,
      apiPolicies
    });

    expect(() => structuredClone(payload)).not.toThrow();
    expect(payload.release.qualityTiers).toEqual([["lossless"], ["320"]]);
    expect(payload.release.mediaTiers).toEqual([["WEB", "CD"], ["Vinyl"]]);
    expect(payload.release.trackerPolicies[0].autoTokenLimit).toBe(100);
    expect(payload.api?.providers.ops).toEqual({ minimumIntervalMs: 3000 });
  });
});
