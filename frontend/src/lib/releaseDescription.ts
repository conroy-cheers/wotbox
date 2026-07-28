import DOMPurify from "dompurify";

const forbiddenTags = [
  "audio",
  "button",
  "embed",
  "form",
  "iframe",
  "input",
  "math",
  "object",
  "option",
  "select",
  "source",
  "style",
  "svg",
  "textarea",
  "video"
];

function isExternalWebUrl(value: string): boolean {
  try {
    const url = new URL(value);
    return url.protocol === "http:" || url.protocol === "https:";
  } catch {
    return false;
  }
}

export function sanitizeReleaseDescription(value: string): string {
  const sanitized = DOMPurify.sanitize(value, {
    USE_PROFILES: { html: true },
    FORBID_TAGS: forbiddenTags,
    FORBID_ATTR: ["class", "id", "srcset", "style"],
    ALLOWED_ATTR: ["alt", "colspan", "height", "href", "rowspan", "src", "title", "width"]
  });
  const template = document.createElement("template");
  template.innerHTML = sanitized;

  for (const link of template.content.querySelectorAll("a")) {
    const href = link.getAttribute("href");
    if (!href || !isExternalWebUrl(href)) {
      link.removeAttribute("href");
      link.removeAttribute("target");
      link.removeAttribute("rel");
      continue;
    }
    link.setAttribute("target", "_blank");
    link.setAttribute("rel", "noopener noreferrer");
  }

  for (const image of template.content.querySelectorAll("img")) {
    const source = image.getAttribute("src");
    if (!source || !isExternalWebUrl(source)) {
      image.remove();
      continue;
    }
    image.setAttribute("loading", "lazy");
    image.setAttribute("referrerpolicy", "no-referrer");
  }

  return template.innerHTML;
}
