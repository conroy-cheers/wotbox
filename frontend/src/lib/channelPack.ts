import type { ChannelPackItem, ChannelPlanSummary } from "./api";

export function executableOrdinals(items: ChannelPackItem[]): Set<number> {
  return new Set(
    items
      .filter((item) => item.planState === "executable")
      .map((item) => item.ordinal)
  );
}

export function summarizeSelection(
  items: ChannelPackItem[],
  selected: ReadonlySet<number>
): ChannelPlanSummary {
  const summary: ChannelPlanSummary = {
    executable: 0,
    skipped: items.length,
    totalSize: 0,
    tokenUses: 0,
    byTracker: {},
    byReason: {}
  };
  for (const item of items) {
    if (item.planState !== "executable" || !item.plan || !selected.has(item.ordinal)) continue;
    summary.executable++;
    summary.skipped--;
    summary.totalSize += item.plan.size ?? 0;
    summary.tokenUses += item.plan.tokenCost;
    summary.byTracker[item.plan.tracker] = (summary.byTracker[item.plan.tracker] ?? 0) + 1;
  }
  return summary;
}
