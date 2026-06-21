// Feature pages — the single source of truth for the nav dropdown, the pages
// themselves, and the sitemap.

export type Feature = {
  slug: string;
  label: string; // nav + card label
  title: string; // page H1
  tagline: string; // one-liner for the nav dropdown
  description: string; // hero + meta description
};

export const features: Feature[] = [
  {
    slug: "materialization",
    label: "Materialization",
    title: "Save a result and query it later",
    tagline: "Turn a query, file, or URL into a reusable table.",
    description:
      "Turn a slow query, local file, or remote URL into a table Pawrly can reuse. Agents get stable data without fetching or rebuilding the same answer every time.",
  },
  {
    slug: "semantic-layer",
    label: "Semantic Layer",
    title: "Give agents the right business vocabulary",
    tagline: "Define approved metrics, joins, and filters once.",
    description:
      "Name the metrics, fields, joins, and filters your team trusts. Then people and agents can ask for revenue, customers, or usage without guessing how your tables work.",
  },
  {
    slug: "observability",
    label: "Observability",
    title: "See what your queries are doing",
    tagline: "See who queried what, what failed, and what is slow.",
    description:
      "See who queried what, what failed, and which sources are slow. Keep a safe query history for people and agents, then send the signals to the monitoring tools you already use.",
  },
];

export const featureBySlug: Record<string, Feature> = Object.fromEntries(
  features.map((f) => [f.slug, f])
);
