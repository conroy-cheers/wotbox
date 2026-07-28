import { describe, expect, it } from "vitest";
import {
  integerSet,
  oneOf,
  optionalPositiveInteger,
  positiveInteger,
  releaseViewPath,
  selectReleaseAttachment,
  viewPath
} from "./routing";

describe("routing helpers", () => {
  it("builds stable, encoded view URLs and supports repeated parameters", () => {
    expect(viewPath("/library", {
      q: "Björk & friends",
      availability: "missing",
      covered: true,
      expanded: [12, 34],
      omitted: ""
    })).toBe(
      "/library?q=Bj%C3%B6rk+%26+friends&availability=missing&covered=1&expanded=12&expanded=34"
    );
  });

  it("normalizes numeric and enumerated route state", () => {
    const params = new URLSearchParams(
      "page=3&torrent=-1&sort=title&expanded=12&expanded=nope&expanded=34"
    );
    expect(positiveInteger(params, "page", 1)).toBe(3);
    expect(optionalPositiveInteger(params, "torrent")).toBeUndefined();
    expect(oneOf(params, "sort", ["year_desc", "title"] as const, "year_desc")).toBe("title");
    expect(integerSet(params, "expanded")).toEqual(new Set([12, 34]));
  });

  it("builds a canonical release URL for an exact download attachment", () => {
    expect(releaseViewPath(
      "ops",
      176023,
      345678,
      "downloads",
      { client: "music client", infoHash: "ABC123" },
      true,
      true
    )).toBe(
      "/releases/ops/176023?torrent=345678&client=music+client&hash=ABC123"
      + "&from=downloads&expanded=176023&details=client"
    );
  });

  it("selects only the requested client attachment and compares hashes case-insensitively", () => {
    const downloads = [
      { client: "archive", infoHash: "ABC123", state: "seeding" },
      { client: "music", infoHash: "abc123", state: "paused" }
    ];
    expect(selectReleaseAttachment(downloads, "music", "ABC123")?.state).toBe("paused");
    expect(selectReleaseAttachment(downloads, "missing", "abc123")).toBeUndefined();
    expect(selectReleaseAttachment(downloads)?.state).toBe("seeding");
  });
});
