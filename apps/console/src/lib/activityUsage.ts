import type { Clients } from "./clients";
import { streamQuery } from "./query";

export interface SourceUsage {
  queries: number;
  /** ISO timestamp of the most recent query touching the source, or null. */
  lastQueried: string | null;
}

const ACTIVITY_SQL =
  "SELECT sql, at FROM system.activity ORDER BY at DESC LIMIT 5000";

/**
 * Per-source query counts + last-queried, scraped from `system.activity` by
 * matching `<source>.` in the (table-name-preserving) redacted `sql` column.
 * Rows arrive newest-first, so the first match per source is its latest.
 * Returns null when the activity table isn't enabled (graceful degradation).
 */
export async function loadSourceUsage(
  query: Clients["query"],
  sourceNames: string[],
): Promise<Record<string, SourceUsage> | null> {
  try {
    const res = await streamQuery(query, ACTIVITY_SQL, 5000, {});
    const sqlIdx = res.columns.indexOf("sql");
    const atIdx = res.columns.indexOf("at");
    if (sqlIdx < 0) return null;

    const usage: Record<string, SourceUsage> = {};
    for (const n of sourceNames) usage[n] = { queries: 0, lastQueried: null };

    for (const row of res.rows) {
      const sql = (row[sqlIdx] ?? "").toLowerCase();
      const at = atIdx >= 0 ? (row[atIdx] ?? "") : "";
      for (const n of sourceNames) {
        if (sql.includes(`${n.toLowerCase()}.`)) {
          const u = usage[n];
          u.queries++;
          if (!u.lastQueried && at) u.lastQueried = at;
        }
      }
    }
    return usage;
  } catch {
    return null;
  }
}
