import { describe, expect, it } from "vitest";
import { formatBytes } from "./api";

describe("formatBytes", () => {
  it("formats tracker byte counts", () => {
    expect(formatBytes(1024)).toBe("1 KiB");
    expect(formatBytes(undefined)).toBe("—");
  });
});
