import type { Metadata } from "next";
import { FeaturePage, FeatureBlock } from "@/components/FeaturePage";
import { CodeBlock } from "@/components/CodeBlock";
import { featureBySlug } from "@/lib/features";

const F = featureBySlug["materialization"];

export const metadata: Metadata = {
  title: F.title,
  description: F.description,
};

const ORIGINS = `# 1 · persist a query result
pawrly materialize top_customers \\
  "SELECT customer, SUM(amount) AS total
   FROM stripe.charges GROUP BY 1
   ORDER BY 2 DESC LIMIT 10"

# 2 · a local file (csv / parquet / json)
pawrly materialize sales --file ./data/sales.csv

# 3 · a remote http(s) file
pawrly materialize prices --url https://ex.com/prices.parquet`;

const QUERY = `pawrly sql "
  SELECT * FROM materialized.top_customers
  ORDER BY total DESC
"`;

const REFRESH = `# re-run the stored origin and overwrite
pawrly cache refresh materialized.sales

# replace by re-materializing the same name
pawrly materialize sales --file ./data/sales-2025.csv

# remove the table and its file
pawrly materialize sales --drop`;

const INLINE = `-- pawrly: materialize big_orders
SELECT * FROM stripe.charges WHERE amount > 1000`;

export default function MaterializationPage() {
  return (
    <FeaturePage title={F.title} description={F.description}>
      <FeatureBlock
        eyebrow="Save the answer"
        title="Keep expensive work from happening twice"
        body="Materialize the output of a query, a local file, or a remote URL when you know you will need it again. Pawrly gives the saved result a table name your team and agents can reuse."
      >
        <CodeBlock lang="bash" title="pawrly materialize" code={ORIGINS} />
      </FeatureBlock>

      <FeatureBlock
        eyebrow="Use it again"
        title="Query it like any other table"
        body="Once a result is saved, use it in normal SQL beside live APIs, files, MCP servers, and databases. The query does not need to know whether the rows came from a fresh source or a saved result."
        reverse
      >
        <CodeBlock lang="bash" title="pawrly sql" code={QUERY} />
      </FeatureBlock>

      <FeatureBlock
        eyebrow="Refresh on purpose"
        title="Update it when the source changes"
        body="A saved table stays stable until you decide to change it. Refresh it to pull the latest data, replace it with a new file or query, or drop it when the snapshot is no longer useful."
      >
        <CodeBlock lang="bash" title="manage" code={REFRESH} />
      </FeatureBlock>

      <FeatureBlock
        eyebrow="Stable by design"
        title="Good for snapshots and agent workflows"
        body={
          <>
            <p>
              Use materialization when repeatability matters: a customer list for a campaign, a
              remote dataset you do not want to fetch on every run, or a query result an agent
              will reference throughout a task.
            </p>
            <p>
              Agents can save a result through the same workspace. With{" "}
              <code className="rounded bg-white/5 px-1.5 py-0.5 font-mono text-[12px] text-gold-2">
                allow_inline
              </code>{" "}
              enabled, a query can return rows and save them for later in the same call.
            </p>
          </>
        }
        reverse
      >
        <CodeBlock lang="sql" title="inline directive" code={INLINE} />
      </FeatureBlock>
    </FeaturePage>
  );
}
