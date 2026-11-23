export type SnapshotRevision = { epoch: string; sequence: number };

const revisions = new WeakMap<object, SnapshotRevision>();

export function parseSnapshotRevision(cursor: string | null): SnapshotRevision | undefined {
  if (!cursor) return undefined;
  const parts = cursor.split(":");
  if (parts.length !== 2 || !parts[0] || !/^\d+$/.test(parts[1])) return undefined;
  const sequence = Number(parts[1]);
  if (!Number.isSafeInteger(sequence)) return undefined;
  return { epoch: parts[0], sequence };
}

export function tagSnapshot(body: unknown, revision: SnapshotRevision): void {
  if (body === null || typeof body !== "object" || revisions.has(body)) return;
  revisions.set(body, revision);
  for (const value of Object.values(body)) {
    tagSnapshot(value, revision);
  }
}

export function snapshotRevision(value: object): SnapshotRevision | undefined {
  return revisions.get(value);
}
