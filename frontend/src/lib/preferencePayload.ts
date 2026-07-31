import type {
  ProviderPolicyOverride,
  QualityPreference,
  RuntimePreferences,
  TrackerPreference,
  VariantSortCriterion
} from "./api";

export interface PreferencePayloadDraft {
  qualityTiers: QualityPreference[][];
  qualityCutoffIndex: number;
  mediaTiers: string[][];
  mediaCutoffIndex: number;
  variantSortOrder: VariantSortCriterion[];
  trackerOrder: string[];
  trackerPolicies: TrackerPreference[];
  apiPolicies: Record<string, ProviderPolicyOverride>;
}

export function buildPreferencePayload(draft: PreferencePayloadDraft): RuntimePreferences {
  return {
    release: {
      qualityTiers: draft.qualityTiers.map((tier) => [...tier]),
      qualityCutoffIndex: draft.qualityCutoffIndex,
      mediaTiers: draft.mediaTiers.map((tier) => [...tier]),
      mediaCutoffIndex: draft.mediaCutoffIndex,
      variantSortOrder: [...draft.variantSortOrder],
      trackerOrder: [...draft.trackerOrder],
      trackerPolicies: draft.trackerPolicies.map((policy) => ({
        ...policy,
        downloadProfile: policy.downloadProfile || undefined,
        autoTokenLimit: Math.max(0, Math.min(100, Number(policy.autoTokenLimit) || 0))
      }))
    },
    api: {
      providers: Object.fromEntries(
        Object.entries(draft.apiPolicies).map(([provider, policy]) => [
          provider,
          { ...policy }
        ])
      )
    }
  };
}
