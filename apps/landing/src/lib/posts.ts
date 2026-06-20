import fs from "node:fs";
import path from "node:path";
import { marked } from "marked";

export type Post = {
  slug: string;
  title: string;
  excerpt: string;
  readTime: string;
  image?: {
    alt: string;
    src: string;
  };
  /** Rendered HTML body (first H1 stripped — it becomes the page title). */
  html: string;
};

// Blog markdown lives inside the landing app so it is bundled with standalone
// deploys. If it isn't present, degrade to no posts rather than breaking builds.
const BLOG_DIR = path.join(process.cwd(), "blogs");

// Curated order — newest / most foundational first.
const ORDER = [
  "agents-need-a-query-surface-not-more-tools",
];

function readRaw(): { slug: string; md: string }[] {
  try {
    return fs
      .readdirSync(BLOG_DIR)
      .filter((f) => f.endsWith(".md"))
      .map((f) => ({
        slug: f.replace(/\.md$/, ""),
        md: fs.readFileSync(path.join(BLOG_DIR, f), "utf8"),
      }));
  } catch {
    return [];
  }
}

function titleFrom(md: string): string {
  const m = md.match(/^#\s+(.+)$/m);
  return m ? m[1].trim() : "Untitled";
}

function excerptFrom(md: string): string {
  const body = md.replace(/^#\s+.+$/m, "").trim();
  const para = body
    .split("\n\n")
    .map((p) => p.trim())
    .find(
      (p) =>
        p &&
        !p.startsWith("#") &&
        !p.startsWith("```") &&
        !p.startsWith(">") &&
        !p.startsWith("![")
    );
  if (!para) return "";
  const clean = para.replace(/[*_`>]/g, "").replace(/\s+/g, " ").trim();
  return clean.length > 180 ? clean.slice(0, 177).trimEnd() + "…" : clean;
}

function imageFrom(md: string): Post["image"] {
  const m = md.match(/^!\[([^\]]*)\]\(([^)]+)\)$/m);
  if (!m) return undefined;
  return {
    alt: m[1].trim(),
    src: m[2].trim(),
  };
}

function readTimeFrom(md: string): string {
  const words = md.split(/\s+/).filter(Boolean).length;
  return `${Math.max(1, Math.round(words / 220))} min read`;
}

export function getPosts(): Post[] {
  const raw = readRaw();
  const posts = raw.map(({ slug, md }) => {
    const bodyMd = md.replace(/^#\s+.+$/m, "").trim();
    return {
      slug,
      title: titleFrom(md),
      excerpt: excerptFrom(md),
      readTime: readTimeFrom(md),
      image: imageFrom(md),
      html: marked.parse(bodyMd, { async: false }) as string,
    };
  });
  posts.sort((a, b) => {
    const ia = ORDER.indexOf(a.slug);
    const ib = ORDER.indexOf(b.slug);
    return (ia === -1 ? 99 : ia) - (ib === -1 ? 99 : ib);
  });
  return posts;
}

export function getPost(slug: string): Post | undefined {
  return getPosts().find((p) => p.slug === slug);
}
