import { describe, expect, it } from "vitest";
import { sanitizeReleaseDescription } from "./releaseDescription";

describe("sanitizeReleaseDescription", () => {
  it("keeps useful release formatting and hardens external links", () => {
    const result = sanitizeReleaseDescription(`
      <h3>Tracklist</h3>
      <ol><li><strong>First track</strong></li></ol>
      <a href="https://example.com/release" style="position:fixed" onclick="alert(1)">Source</a>
    `);

    expect(result).toContain("<h3>Tracklist</h3>");
    expect(result).toContain("<strong>First track</strong>");
    expect(result).toContain('href="https://example.com/release"');
    expect(result).toContain('target="_blank"');
    expect(result).toContain('rel="noopener noreferrer"');
    expect(result).not.toContain("style=");
    expect(result).not.toContain("onclick");
  });

  it("removes executable content, unsafe images, and misleading relative links", () => {
    const result = sanitizeReleaseDescription(`
      <script>alert("no")</script>
      <iframe src="https://example.com"></iframe>
      <a class="sidebar" href="artist.php?id=1">Artist</a>
      <img src="data:image/svg+xml,bad">
      <img src="https://images.example.com/cover.jpg" onerror="alert(1)">
    `);

    expect(result).not.toContain("<script");
    expect(result).not.toContain("<iframe");
    expect(result).not.toContain("class=");
    expect(result).toContain("<a>Artist</a>");
    expect(result).not.toContain("data:image");
    expect(result).toContain('src="https://images.example.com/cover.jpg"');
    expect(result).toContain('loading="lazy"');
    expect(result).toContain('referrerpolicy="no-referrer"');
    expect(result).not.toContain("onerror");
  });
});
