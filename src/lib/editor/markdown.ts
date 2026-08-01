import DOMPurify from "dompurify";
import MarkdownIt from "markdown-it";

const SAFE_IMAGE_URL = /^(?:blob:|data:image\/(?:png|gif|jpe?g|webp);base64,|\.\.?\/|\/)/i;
const SAFE_LINK_URL = /^(?:https?:|mailto:|tel:|#|\.\.?\/|\/)/i;

export interface RenderMarkdownOptions {
  assetUrls?: Readonly<Record<string, string>>;
}

type MarkdownTokens = Parameters<MarkdownIt["inline"]["parse"]>[3];

interface MarkdownInlineState {
  src: string;
  pos: number;
  posMax: number;
  env: unknown;
  md: MarkdownIt;
  tokens: MarkdownTokens;
  push(type: string, tag: string, nesting: -1 | 0 | 1): MarkdownTokens[number];
}

function escapedAt(source: string, position: number): boolean {
  let backslashes = 0;
  for (let index = position - 1; index >= 0 && source[index] === "\\"; index -= 1) {
    backslashes += 1;
  }
  return backslashes % 2 === 1;
}

function findHighlightEnd(source: string, from: number, limit: number): number {
  let position = source.indexOf("==", from);
  while (position >= 0 && position < limit) {
    if (source.slice(from, position).includes("\n")) return -1;
    if (position > from && !/\s/.test(source[position - 1]) && !escapedAt(source, position)) {
      return position;
    }
    position = source.indexOf("==", position + 2);
  }
  return -1;
}

function highlightInlineRule(state: MarkdownInlineState, silent: boolean): boolean {
  const start = state.pos;
  const contentStart = start + 2;
  if (
    state.src.slice(start, contentStart) !== "=="
    || state.src[contentStart] === "="
    || !state.src[contentStart]
    || /\s/.test(state.src[contentStart])
  ) {
    return false;
  }

  const end = findHighlightEnd(state.src, contentStart, state.posMax);
  if (end < 0) return false;

  state.pos = end + 2;
  if (silent) return true;

  const opening = state.push("mark_open", "mark", 1);
  opening.markup = "==";
  state.md.inline.parse(state.src.slice(contentStart, end), state.md, state.env, state.tokens);
  const closing = state.push("mark_close", "mark", -1);
  closing.markup = "==";
  return true;
}

function normalizeAssetPath(source: string): string {
  const hash = source.indexOf("#");
  const query = source.indexOf("?");
  const end = Math.min(hash < 0 ? source.length : hash, query < 0 ? source.length : query);
  const path = source.slice(0, end).replaceAll("\\", "/");
  try {
    return decodeURIComponent(path);
  } catch {
    return path;
  }
}

function resolveImageSource(
  source: string,
  assetUrls: Readonly<Record<string, string>>,
): string | null {
  const normalized = normalizeAssetPath(source);
  const mapped = assetUrls[source] ?? assetUrls[normalized];
  if (mapped && SAFE_IMAGE_URL.test(mapped) && !mapped.toLowerCase().startsWith("file:")) {
    return mapped;
  }

  if (source.toLowerCase().startsWith("data:image/") && SAFE_IMAGE_URL.test(source)) {
    return source;
  }

  return null;
}

function createRenderer(assetUrls: Readonly<Record<string, string>>): MarkdownIt {
  const parser = new MarkdownIt({
    breaks: true,
    html: false,
    linkify: true,
    typographer: false,
  });

  parser.inline.ruler.before("emphasis", "highlight", highlightInlineRule);

  parser.validateLink = (url: string) => {
    const normalized = url.trim();
    const hasScheme = /^[a-z][a-z0-9+.-]*:/i.test(normalized);
    return SAFE_LINK_URL.test(normalized) || (!hasScheme && !normalized.startsWith("//"));
  };

  parser.renderer.rules.link_open = (tokens, index, options, environment, renderer) => {
    const token = tokens[index];
    token.attrSet("rel", "noopener noreferrer");
    token.attrSet("target", "_blank");
    return renderer.renderToken(tokens, index, options);
  };

  parser.renderer.rules.list_item_open = (tokens, index, options, environment, renderer) => {
    const inline = tokens.slice(index + 1, index + 4).find((token) => token.type === "inline");
    const marker = inline?.type === "inline" ? /^\[([ xX])\]\s+/.exec(inline.content) : null;
    const firstText = inline?.children?.[0];

    if (!inline || !marker || firstText?.type !== "text") {
      return renderer.renderToken(tokens, index, options);
    }

    inline.content = inline.content.slice(marker[0].length);
    firstText.content = firstText.content.slice(marker[0].length);
    const checked = marker[1].toLowerCase() === "x" ? " checked" : "";
    return `<li class="task-list-item"><input type="checkbox" disabled${checked}>`;
  };

  parser.renderer.rules.image = (tokens, index) => {
    const token = tokens[index];
    const rawSource = token.attrGet("src") ?? "";
    const source = resolveImageSource(rawSource, assetUrls);
    const alt = parser.utils.escapeHtml(token.content || "图片");
    const title = token.attrGet("title");

    if (!source) {
      return `<span class="md-image-placeholder" data-path="${parser.utils.escapeHtml(rawSource)}" role="img" aria-label="${alt}">🖼 ${alt}</span>`;
    }

    const titleAttribute = title ? ` title="${parser.utils.escapeHtml(title)}"` : "";
    return `<img src="${parser.utils.escapeHtml(source)}" alt="${alt}"${titleAttribute} loading="lazy" decoding="async">`;
  };

  for (const ruleName of ["th_open", "td_open"] as const) {
    parser.renderer.rules[ruleName] = (tokens, index) => {
      const token = tokens[index];
      const alignment = /text-align\s*:\s*(left|center|right)/i.exec(token.attrGet("style") ?? "")?.[1]
        ?.toLowerCase();
      const alignmentClass = alignment ? ` class="md-align-${alignment}"` : "";
      return `<${token.tag}${alignmentClass}>`;
    };
  }

  return parser;
}

export function renderMarkdown(
  markdown: string,
  options: RenderMarkdownOptions = {},
): string {
  const html = createRenderer(options.assetUrls ?? {}).render(markdown);

  if (typeof window === "undefined" || !DOMPurify.isSupported) {
    return html;
  }

  return DOMPurify.sanitize(html, {
    ALLOWED_TAGS: [
      "a",
      "blockquote",
      "br",
      "code",
      "del",
      "em",
      "h1",
      "h2",
      "h3",
      "h4",
      "h5",
      "h6",
      "hr",
      "img",
      "input",
      "li",
      "mark",
      "ol",
      "p",
      "pre",
      "s",
      "span",
      "strong",
      "table",
      "tbody",
      "td",
      "th",
      "thead",
      "tr",
      "ul",
    ],
    ALLOWED_ATTR: [
      "alt",
      "aria-label",
      "checked",
      "class",
      "data-path",
      "decoding",
      "disabled",
      "href",
      "loading",
      "rel",
      "role",
      "src",
      "target",
      "title",
      "type",
    ],
    ALLOW_DATA_ATTR: true,
  });
}
