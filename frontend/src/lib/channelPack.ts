import type { ChannelPackItem, ChannelPlanSummary } from "./api";

export function executableOrdinals(items: ChannelPackItem[]): Set<number> {
  return new Set(
    items
      .filter((item) =>
        item.planState === "executable"
        || (Boolean(item.replacement)
          && ["cleanup_ready", "already_downloading"].includes(item.planState)))
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
    if (!selected.has(item.ordinal)) continue;
    const actionable = item.planState === "executable"
      || (Boolean(item.replacement)
        && ["cleanup_ready", "already_downloading"].includes(item.planState));
    if (!actionable) continue;
    summary.executable++;
    summary.skipped--;
    summary.totalSize += item.plan?.size ?? 0;
    summary.tokenUses += item.plan?.tokenCost ?? 0;
    const tracker = item.replacement?.tracker ?? item.plan?.tracker;
    if (tracker) summary.byTracker[tracker] = (summary.byTracker[tracker] ?? 0) + 1;
  }
  return summary;
}
