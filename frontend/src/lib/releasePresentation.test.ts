import { describe, expect, it } from "vitest";
import { releaseTypeColor } from "./releasePresentation";

describe("release type colours", () => {
  it("keeps common release types visually distinct", () => {
    expect(releaseTypeColor("Album")).not.toBe(releaseTypeColor("Single"));
    expect(releaseTypeColor("EP")).not.toBe(releaseTypeColor("Live Album"));
    expect(releaseTypeColor("Demo")).not.toBe(releaseTypeColor("Anthology"));
  });

  it("uses a stable neutral colour for missing or unknown types", () => {
    expect(releaseTypeColor()).toBe("#697083");
    expect(releaseTypeColor("Something unusual")).toBe("#697083");
  });
});
