const releaseTypeColors: Array<[RegExp, string]> = [
  [/\bsingle\b/i, "#54c6d4"],
  [/\bep\b/i, "#e879c5"],
  [/\bcompilation\b|\bsampler\b/i, "#e5aa52"],
  [/\bsoundtrack\b/i, "#ed855c"],
  [/\blive\b|\bconcert\b/i, "#5b9ee8"],
  [/\bremix/i, "#68c99a"],
  [/\bdemo\b/i, "#e46f79"],
  [/\banthology\b/i, "#d5bd62"],
  [/\bmixtape\b|\bdj mix\b/i, "#7bc07b"],
  [/\bbootleg\b/i, "#b58ad7"],
  [/\balbum\b/i, "#9b87f5"]
];

export function releaseTypeColor(releaseType?: string): string {
  if (!releaseType) return "#697083";
  return releaseTypeColors.find(([pattern]) => pattern.test(releaseType))?.[1] ?? "#697083";
}
