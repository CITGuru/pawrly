import type { Metadata } from "next";
import { FeaturePage, FeatureBlock } from "@/components/FeaturePage";
import { CodeBlock } from "@/components/CodeBlock";
import { featureBySlug } from "@/lib/features";

const F = featureBySlug["observability"];

export const metadata: Metadata = {
  title: F.title,
  description: F.description,
};

const ENABLE = `# inspect one run
pawrly --log-format json sql "SELECT 1"

# watch a shared workspace
pawrly --otel-endpoint http://localhost:4317 serve

# expose metrics directly
pawrly --prometheus-listen 127.0.0.1:9090 serve`;

const ACTIVITY = `SELECT interface, status,
       count(*)         AS n,
       avg(duration_ms) AS avg_ms
FROM system.activity
WHERE at > now() - INTERVAL '1 hour'
GROUP BY 1, 2
ORDER BY n DESC;`;

const REDACT = `observability:
  activity:
    enabled: true
    sinks: [tracing, table]
    redact_sql: literals       # keep the query shape, remove values
    store: ~/.pawrly/activity  # keep history across restarts
    retention: 30d`;

const SIGNALS = `queries_total{status="ok"}              1284
query_duration_ms{p95="true"}           84
source_requests_total{kind="http"}      942
source_errors_total{source="stripe"}    3`;

export default function ObservabilityPage() {
  return (
    <FeaturePage title={F.title} description={F.description}>
      <FeatureBlock
        eyebrow="Off by default"
        title="Start quiet. Add visibility when it matters."
        body="Pawrly can stay quiet for local experiments. When a workspace is shared with teammates, scripts, or agents, turn on activity history and monitoring so you can see what ran, where it came from, and whether it worked."
      >
        <CodeBlock lang="bash" title="enable" code={ENABLE} />
      </FeatureBlock>

      <FeatureBlock
        eyebrow="Audit"
        title="Ask what happened"
        body="When activity logging is on, every query leaves a row: who ran it, where it came from, whether it failed, how long it took, and how many rows came back. You can inspect that history with SQL instead of searching through logs."
        reverse
      >
        <CodeBlock lang="sql" title="system.activity" code={ACTIVITY} />
      </FeatureBlock>

      <FeatureBlock
        eyebrow="Leak-safe"
        title="Keep history without keeping secrets"
        body={
          <>
            <p>
              Query history is useful only if it is safe to keep. Pawrly can preserve the shape of
              a query while removing the sensitive values inside it.
            </p>
            <p>
              Store full SQL when you want it, replace literal values with{" "}
              <code className="rounded bg-white/5 px-1.5 py-0.5 font-mono text-[12px] text-gold-2">
                $REDACTED
              </code>
              , or keep only the statement type and the tables touched. Parameter values stay out
              of the activity log.
            </p>
          </>
        }
      >
        <CodeBlock lang="yaml" title="pawrly.yaml" code={REDACT} />
      </FeatureBlock>

      <FeatureBlock
        eyebrow="Export"
        title="Use the monitoring stack you already have"
        body="Send query counts, durations, row counts, source errors, and request traces to Grafana, Datadog, or the monitoring system your team already trusts. Pawrly exports through OpenTelemetry and Prometheus when you need those integrations."
        reverse
      >
        <CodeBlock lang="bash" title="signals" code={SIGNALS} />
      </FeatureBlock>
    </FeaturePage>
  );
}
