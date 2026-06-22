import Link from "next/link";
import { docList } from "@/lib/docs-config";

/** Previous / next page links at the foot of a doc, ordered by the docs nav. */
export function DocsPrevNext({ slug }: { slug: string }) {
  const idx = docList.findIndex((d) => d.slug === slug);
  if (idx === -1) return null;
  const prev = idx > 0 ? docList[idx - 1] : null;
  const next = idx < docList.length - 1 ? docList[idx + 1] : null;
  if (!prev && !next) return null;

  return (
    <nav
      aria-label="Pagination"
      className="mt-16 grid gap-4 border-t border-line pt-8 sm:grid-cols-2"
    >
      {prev ? (
        <Link
          href={`/docs/${prev.slug}`}
          className="card card-hover flex flex-col gap-1 rounded-2xl p-4"
        >
          <span className="text-xs text-muted-2">← Previous</span>
          <span className="text-sm font-medium text-cream">{prev.title}</span>
        </Link>
      ) : (
        <span className="hidden sm:block" />
      )}
      {next ? (
        <Link
          href={`/docs/${next.slug}`}
          className="card card-hover flex flex-col gap-1 rounded-2xl p-4 sm:items-end sm:text-right"
        >
          <span className="text-xs text-muted-2">Next →</span>
          <span className="text-sm font-medium text-cream">{next.title}</span>
        </Link>
      ) : null}
    </nav>
  );
}
