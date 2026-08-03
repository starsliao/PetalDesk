import DOMPurify from "dompurify";
import MarkdownIt from "markdown-it";

const SAFE_IMAGE_URL = /^(?:https?:\/\/|blob:|data:image\/(?:png|gif|jpe?g|webp);base64,|\.\.?\/|\/)/i;
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

interface HtmlImageAttributes {
  src: string;
  alt?: string;
  title?: string;
  width?: string;
  height?: string;
}

type MarkdownToken = ReturnType<MarkdownIt["parse"]>[number];

const HTML_IMAGE_ATTRIBUTES = new Set(["src", "alt", "title", "width", "height"]);

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
  const trimmed = source.trim();
  const normalized = normalizeAssetPath(source);
  const mapped = assetUrls[source] ?? assetUrls[normalized];
  if (mapped && SAFE_IMAGE_URL.test(mapped) && !mapped.toLowerCase().startsWith("file:")) {
    return mapped;
  }

  if (
    (/^https?:\/\//i.test(trimmed) || trimmed.toLowerCase().startsWith("data:image/"))
    && SAFE_IMAGE_URL.test(trimmed)
  ) {
    return trimmed;
  }

  return null;
}

function parseHtmlImageTag(source: string): HtmlImageAttributes | null {
  const tag = source.trim();
  const opening = /^<img\b/i.exec(tag);
  if (!opening) return null;

  const attributes: Partial<HtmlImageAttributes> = {};
  let position = opening[0].length;

  while (position < tag.length) {
    while (/\s/.test(tag[position] ?? "")) position += 1;

    if (tag[position] === ">") {
      position += 1;
      break;
    }
    if (tag[position] === "/" && tag[position + 1] === ">") {
      position += 2;
      break;
    }

    const nameMatch = /^[A-Za-z_:][A-Za-z0-9:._-]*/.exec(tag.slice(position));
    if (!nameMatch) return null;
    const name = nameMatch[0].toLowerCase();
    position += nameMatch[0].length;
    while (/\s/.test(tag[position] ?? "")) position += 1;

    let value = "";
    if (tag[position] === "=") {
      position += 1;
      while (/\s/.test(tag[position] ?? "")) position += 1;
      const quote = tag[position];
      if (quote === '"' || quote === "'") {
        const end = tag.indexOf(quote, position + 1);
        if (end < 0) return null;
        value = tag.slice(position + 1, end);
        position = end + 1;
      } else {
        const valueMatch = /^[^\s"'=<>`]+/.exec(tag.slice(position));
        if (!valueMatch) return null;
        value = valueMatch[0];
        position += value.length;
      }
    }

    if (HTML_IMAGE_ATTRIBUTES.has(name)) {
      attributes[name as keyof HtmlImageAttributes] = value;
    }
  }

  if (position !== tag.length || !attributes.src) return null;
  return attributes as HtmlImageAttributes;
}

export function isSupportedHtmlImageTag(source: string): boolean {
  return parseHtmlImageTag(source) !== null;
}

function safeImageDimension(value: string | undefined): string | undefined {
  if (!value || !/^\d{1,5}$/.test(value)) return undefined;
  const dimension = Number(value);
  return dimension >= 1 && dimension <= 4096 ? String(dimension) : undefined;
}

function renderImage(
  parser: MarkdownIt,
  rawSource: string,
  rawAlt: string,
  rawTitle: string | null,
  assetUrls: Readonly<Record<string, string>>,
  dimensions: Pick<HtmlImageAttributes, "width" | "height"> = {},
): string {
  const source = resolveImageSource(rawSource, assetUrls);
  const alt = parser.utils.escapeHtml(rawAlt || "图片");

  if (!source) {
    return `<span class="md-image-placeholder" data-path="${parser.utils.escapeHtml(rawSource)}" role="img" aria-label="${alt}">🖼 ${alt}</span>`;
  }

  const titleAttribute = rawTitle ? ` title="${parser.utils.escapeHtml(rawTitle)}"` : "";
  const width = safeImageDimension(dimensions.width);
  const height = safeImageDimension(dimensions.height);
  const widthAttribute = width ? ` width="${width}"` : "";
  const heightAttribute = height ? ` height="${height}"` : "";
  return `<img src="${parser.utils.escapeHtml(source)}" alt="${alt}"${titleAttribute}${widthAttribute}${heightAttribute} loading="lazy" decoding="async" referrerpolicy="no-referrer">`;
}

function renderSafeHtmlImage(
  parser: MarkdownIt,
  source: string,
  assetUrls: Readonly<Record<string, string>>,
): string | null {
  const attributes = parseHtmlImageTag(source);
  if (!attributes) return null;
  const imageSource = parser.utils.unescapeAll(attributes.src.trim());
  const alt = parser.utils.unescapeAll(attributes.alt ?? "图片");
  const title = attributes.title === undefined
    ? null
    : parser.utils.unescapeAll(attributes.title);
  return renderImage(parser, imageSource, alt, title, assetUrls, attributes);
}

function localAssetPath(source: string): string | null {
  const normalized = normalizeAssetPath(source);
  const segments = normalized.split("/");
  return segments[0]?.toLowerCase() === "assets"
    && segments.length === 2
    && Boolean(segments[1])
    && segments[1] !== ".."
    && segments[1] !== "."
    ? normalized
    : null;
}

function createRenderer(assetUrls: Readonly<Record<string, string>>): MarkdownIt {
  const parser = new MarkdownIt({
    breaks: true,
    html: true,
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
    return renderImage(
      parser,
      rawSource,
      token.content || "图片",
      token.attrGet("title"),
      assetUrls,
    );
  };

  parser.renderer.rules.html_inline = (tokens, index) => {
    const source = tokens[index].content;
    return renderSafeHtmlImage(parser, source, assetUrls) ?? parser.utils.escapeHtml(source);
  };

  parser.renderer.rules.html_block = (tokens, index) => {
    const source = tokens[index].content;
    const image = renderSafeHtmlImage(parser, source, assetUrls);
    return image ? `${image}\n` : parser.utils.escapeHtml(source);
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

export function extractLocalImagePaths(markdown: string): string[] {
  const parser = createRenderer({});
  const paths = new Set<string>();

  const visit = (tokens: readonly MarkdownToken[]): void => {
    for (const token of tokens) {
      let source: string | null = null;
      if (token.type === "image") {
        source = token.attrGet("src");
      } else if (token.type === "html_inline" || token.type === "html_block") {
        const attributes = parseHtmlImageTag(token.content);
        source = attributes ? parser.utils.unescapeAll(attributes.src.trim()) : null;
      }

      if (source) {
        const path = localAssetPath(source);
        if (path) paths.add(path);
      }
      if (token.children) visit(token.children);
    }
  };

  visit(parser.parse(markdown, {}));
  return [...paths];
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
      "height",
      "loading",
      "rel",
      "referrerpolicy",
      "role",
      "src",
      "target",
      "title",
      "type",
      "width",
    ],
    ALLOW_DATA_ATTR: true,
  });
}
