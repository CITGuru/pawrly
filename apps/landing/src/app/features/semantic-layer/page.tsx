import type { Metadata } from "next";
import { FeaturePage, FeatureBlock } from "@/components/FeaturePage";
import { CodeBlock } from "@/components/CodeBlock";
import { featureBySlug } from "@/lib/features";

const F = featureBySlug["semantic-layer"];

export const metadata: Metadata = {
  title: F.title,
  description: F.description,
};

const MODEL = `semantic:
  models:
    - name: orders
      source: data.orders
      primary_key: [id]
      dimensions:
        - { name: status,     expr: status,     type: string }
        - { name: order_date, expr: ordered_at, type: time,
            grains: [day, week, month, quarter, year] }
      measures:
        - { name: order_count,  agg: count_distinct, expr: id }
        - { name: revenue,      agg: sum, expr: total_amount }
        - { name: paid_revenue, agg: sum, expr: total_amount,
            filters: ["status = 'paid'"] }`;

const QUERY = `pawrly semantic query orders.revenue orders.order_count \\
  --by orders.status \\
  --by orders.order_date.month \\
  --where 'orders.status = paid' \\
  --order-by orders.revenue:desc`;

const GUARDS = `# This join would double-count revenue,
# so Pawrly blocks it instead of guessing.
pawrly semantic query orders.revenue --by order_items.sku

# Required filters are applied every time.
pawrly semantic query orders.revenue --by orders.status \\
  --param tenant_id=acme`;

const PREAGG = `pre_aggregations:
  - name: daily_by_status
    dimensions: [order_date.day, status]
    measures:   [revenue, order_count]
    refresh:    1h
    partition_by: order_date.month`;

export default function SemanticLayerPage() {
  return (
    <FeaturePage title={F.title} description={F.description}>
      <FeatureBlock
        eyebrow="Name what matters"
        title="Give business concepts a home"
        body="Define the fields and metrics your team already talks about — revenue, order count, status, customer, usage — and keep those definitions in config instead of repeating them in every prompt or query."
      >
        <CodeBlock lang="yaml" title="pawrly.yaml" code={MODEL} />
      </FeatureBlock>

      <FeatureBlock
        eyebrow="Ask clearly"
        title="Ask for metrics, not table plumbing"
        body="People and agents can ask for revenue by status or order count by month without knowing the raw table layout. Pawrly turns those approved names into the SQL needed to answer the question."
        reverse
      >
        <CodeBlock lang="bash" title="pawrly semantic" code={QUERY} />
      </FeatureBlock>

      <FeatureBlock
        eyebrow="Guarded"
        title="Block bad answers before they ship"
        body={
          <>
            <p>
              Some joins look reasonable but quietly double-count revenue or usage. Pawrly can
              block those queries before an agent turns a bad number into a confident answer.
            </p>
            <p>
              Required filters travel with the model too, so customer, tenant, or workspace
              boundaries are applied every time the metric is queried.
            </p>
          </>
        }
      >
        <CodeBlock lang="bash" title="guardrails" code={GUARDS} />
      </FeatureBlock>

      <FeatureBlock
        eyebrow="Fast"
        title="Speed up common questions"
        body="For metrics people ask about often, define a prepared rollup once. Pawrly can answer matching questions from the faster saved result and fall back to the source data when it needs the full detail."
        reverse
      >
        <CodeBlock lang="yaml" title="pre_aggregations" code={PREAGG} />
      </FeatureBlock>
    </FeaturePage>
  );
}
