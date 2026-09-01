import { describe, expect, it } from "vitest";
import type { Query } from "@tanstack/svelte-query";
import { isLiveQuery, queryUsesResources, queryUsesScopes } from "./live";

function query(key: unknown[]): Query {
  return { queryKey: key } as unknown as Query;
}

describe("live query catalog", () => {
  it("keeps immutable runtime configuration out of invalidation", () => {
    expect(isLiveQuery(query(["config"]))).toBe(false);
    expect(isLiveQuery(query(["download-profiles"]))).toBe(false);
  });

  it("maps activity changes only to dependent queries", () => {
    const scopes = new Set(["activity"]);
    expect(queryUsesScopes(query(["downloads"]), scopes)).toBe(true);
    expect(queryUsesScopes(query(["channel-pack", "id"]), scopes)).toBe(true);
    expect(queryUsesScopes(query(["providers"]), scopes)).toBe(false);
  });

  it("does not broadly invalidate queries with no declared dependency", () => {
    expect(queryUsesScopes(query(["future-page"]), new Set(["catalog"]))).toBe(false);
  });

  it("maps durable resources independently of broad compatibility scopes", () => {
    expect(queryUsesResources(query(["library"]), new Set(["library"]))).toBe(true);
    expect(queryUsesResources(query(["downloads"]), new Set(["downloads"]))).toBe(false);
    expect(
      queryUsesResources(query(["downloads"]), new Set(["download-inventory"]))
    ).toBe(true);
    expect(queryUsesResources(query(["library"]), new Set(["catalog"]))).toBe(false);
    expect(queryUsesResources(query(["channel-pack"]), new Set(["channels"]))).toBe(true);
  });
});
