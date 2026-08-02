import type { ProviderStatus } from "./api";

export function providerStatusSummary(provider: ProviderStatus): string {
  if (provider.state === "cooldown" && provider.reasonCode === "rate_limited") {
    return provider.retryAt
      ? `${provider.displayName} is temporarily rate-limited until ${new Date(provider.retryAt).toLocaleString()}`
      : `${provider.displayName} is temporarily rate-limited`;
  }
  if (provider.state === "blocked" && provider.reasonCode === "authentication_failed") {
    return `${provider.displayName} authentication needs attention`;
  }
  if (provider.state === "blocked") return `${provider.displayName} is blocked`;
  if (provider.state === "paused") return `${provider.displayName} is paused`;
  if (provider.state === "half_open") return `${provider.displayName} is testing connectivity`;
  return `${provider.displayName} is available`;
}

export function providerNeedsAttention(provider?: ProviderStatus): boolean {
  return Boolean(provider && ["blocked", "paused"].includes(provider.state));
}
