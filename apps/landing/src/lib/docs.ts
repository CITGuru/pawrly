import fs from "node:fs";
import path from "node:path";
import { Marked } from "marked";
import { createHighlighter, type Highlighter } from "shiki";
import { docList, validSlugs, type TocEntry } from "./docs-config";

export type Doc = {
  slug: string;
  title: string;
  html: string;
  toc: TocEntry[];
  raw: string;
};

const DOCS_DIR = path.join(process.cwd(), "public", "docs");

// ---- shiki (build-time, singleton) ----------------------------------------
const THEME = "poimandres";
const LANGS = ["yaml", "bash", "json", "sql", "powershell"];
let highlighterPromise: Promise<Highlighter> | null = null;
function getHighlighter() {
  if (!highlighterPromise) {
    highlighterPromise = createHighlighter({ themes: [THEME], langs: LANGS });
  }
  return highlighterPromise;
}
function normalizeLang(lang?: string): string {
  const l = (lang || "").toLowerCase();
  const alias: Record<string, string> = {
    sh: "bash",
    shell: "bash",
    zsh: "bash",
    console: "bash",
    yml: "yaml",
    ps1: "powershell",
    ps: "powershell",
  };
  const norm = alias[l] ?? l;
  return LANGS.includes(norm) ? norm : "text";
}

// ---- helpers --------------------------------------------------------------
function escapeHtml(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

function stripInlineMd(s: string): string {
  return s
    .replace(/`([^`]+)`/g, "$1")
    .replace(/\*\*?([^*]+)\*\*?/g, "$1")
    .replace(/\[([^\]]+)\]\([^)]+\)/g, "$1")
    .trim();
}

function slugifyHeading(text: string): string {
  return stripInlineMd(text)
    .toLowerCase()
    .replace(/[^\w\s-]/g, "")
    .replace(/\s+/g, "-")
    .replace(/-+/g, "-")
    .replace(/^-|-$/g, "");
}

// Rewrite intra-doc links — ./sources.md#caching → /docs/sources#caching — for
// known doc slugs only; external/other links are left untouched.
function rewriteLinks(md: string): string {
  return md.replace(
    /\]\((?:\.\/)?([a-z0-9-]+)\.md(#[^)]*)?\)/gi,
    (match, slug, anchor = "") =>
      validSlugs.has(slug) ? `](/docs/${slug}${anchor || ""})` : match
  );
}

function extractToc(md: string): TocEntry[] {
  const tokens = new Marked().lexer(md);
  const toc: TocEntry[] = [];
  for (const t of tokens) {
    if (t.type === "heading" && (t.depth === 2 || t.depth === 3)) {
      toc.push({
        id: slugifyHeading(t.text),
        text: stripInlineMd(t.text),
        depth: t.depth as 2 | 3,
      });
    }
  }
  return toc;
}

async function renderMarkdown(md: string): Promise<string> {
  const hl = await getHighlighter();
  const marked = new Marked({ gfm: true });
  marked.use({
    walkTokens(token) {
      if (token.type === "code") {
        const lang = normalizeLang(token.lang);
        const original = token.text;
        try {
          token.text = hl.codeToHtml(original, { lang, theme: THEME });
        } catch {
          token.text = `<pre class="shiki"><code>${escapeHtml(original)}</code></pre>`;
        }
      }
    },
    renderer: {
      code({ text }) {
        // `text` is already the full shiki <pre> markup.
        return `<div class="code-block">${text}</div>`;
      },
      heading({ tokens, depth }) {
        const inner = this.parser.parseInline(tokens);
        const id = slugifyHeading(inner.replace(/<[^>]+>/g, ""));
        return `<h${depth} id="${id}"><a class="heading-anchor" href="#${id}">${inner}</a></h${depth}>\n`;
      },
    },
  });
  return marked.parse(md) as string;
}

// ---- public API -----------------------------------------------------------
function readRaw(slug: string): string | null {
  if (!validSlugs.has(slug)) return null;
  try {
    return fs.readFileSync(path.join(DOCS_DIR, `${slug}.md`), "utf8");
  } catch {
    return null;
  }
}

export async function getDoc(slug: string): Promise<Doc | null> {
  const raw = readRaw(slug);
  if (raw === null) return null;
  const meta = docList.find((d) => d.slug === slug);
  const bodyMd = raw.replace(/^#\s+.+$/m, "").trim();
  const html = await renderMarkdown(rewriteLinks(bodyMd));
  return {
    slug,
    title: meta?.title ?? slug,
    html,
    toc: extractToc(bodyMd),
    raw,
  };
}

// ---- search index (build-time) --------------------------------------------
export type SearchEntry = {
  slug: string;
  doc: string;
  title: string; // section heading, or the doc title for the page-level entry
  id: string; // heading anchor ("" for the page-level entry)
  text: string; // plain-text snippet of the section
};

function buildSections(raw: string, doc: { slug: string; title: string }): SearchEntry[] {
  const out: SearchEntry[] = [];
  let cur = { title: doc.title, id: "", buf: [] as string[] };
  let inFence = false;

  const flush = () => {
    const text = cur.buf
      .join(" ")
      .replace(/`/g, "")
      .replace(/\[([^\]]+)\]\([^)]+\)/g, "$1")
      .replace(/[*_>#]/g, " ")
      .replace(/\s+/g, " ")
      .trim()
      .slice(0, 300);
    out.push({ slug: doc.slug, doc: doc.title, title: cur.title, id: cur.id, text });
  };

  for (const line of raw.split("\n")) {
    if (/^```/.test(line)) {
      inFence = !inFence;
      continue;
    }
    if (inFence) continue;
    const h = line.match(/^(#{1,3})\s+(.*)$/);
    if (h) {
      if (h[1].length === 1) continue; // doc title — keep accumulating the intro
      flush();
      cur = { title: stripInlineMd(h[2]), id: slugifyHeading(h[2]), buf: [] };
      continue;
    }
    if (line.trim()) cur.buf.push(line.trim());
  }
  flush();
  return out;
}

export function getSearchIndex(): SearchEntry[] {
  const out: SearchEntry[] = [];
  for (const meta of docList) {
    const raw = readRaw(meta.slug);
    if (raw) out.push(...buildSections(raw, meta));
  }
  return out;
}
