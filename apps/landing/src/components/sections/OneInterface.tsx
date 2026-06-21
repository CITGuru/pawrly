import { SectionHeader } from "../UI";
import { CodeBlock } from "../CodeBlock";
import { QueryCard } from "../QueryCard";

const YAML = `version: 1
name: my-workspace
secrets:
  - kind: env

sources:
  - name: stripe
    kind: http
    config:
      base_url: https://api.stripe.com
      auth:
        type: header
        headers:
          - name: Authorization
            bearer: \${secret:STRIPE_API_KEY}
    tables:
      - name: customers
        endpoint: /v1/customers
        response:
          path: $.data
          schema:
            - { name: email,      type: varchar }
            - { name: name,       type: varchar }
            - { name: delinquent, type: bool }

  - name: intercom
    kind: http
    config:
      base_url: https://api.intercom.io
      auth:
        type: header
        headers:
          - name: Authorization
            bearer: \${secret:INTERCOM_TOKEN}
    tables:
      - name: contacts
        endpoint: /contacts
        response:
          path: $.data
          schema:
            - { name: email,        type: varchar }
            - { name: last_seen_at, type: bigint }`;

export function OneInterface() {
  return (
    <section id="how" className="relative scroll-mt-24 py-24 md:py-32">
      <div className="mx-auto max-w-6xl px-6">
        <SectionHeader
          eyebrow="How it works"
          title={
            <>
              Describe each source once.
              <br className="hidden md:block" /> Query it as SQL forever.
            </>
          }
          description="Add the source to pawrly.yaml, give the fields clear names, and query it like a table. The next person, script, or agent uses the same names without learning another API."
        />

        <div className="mt-14 grid gap-5 lg:grid-cols-[1.15fr_1fr]">
          {/* The spec is taller than the column; pull the card out of flow so the
              right column (query + note) sets the row height and the YAML fills
              it and scrolls. Fixed height on mobile where there's no sibling. */}
          <div className="relative h-[520px] lg:h-auto">
            <CodeBlock
              lang="yaml"
              title="pawrly.yaml"
              code={YAML}
              className="absolute inset-0 flex flex-col"
              bodyClassName="min-h-0 flex-1 overflow-y-auto"
            />
          </div>

          <div className="flex flex-col gap-5">
            <QueryCard />

            <div className="card rounded-2xl p-6">
              <p className="text-sm leading-relaxed text-muted">
                Run the same query from the CLI, an MCP client, or a long-running{" "}
                <code className="rounded bg-white/5 px-1.5 py-0.5 font-mono text-[12px] text-gold-2">
                  pawrly serve
                </code>{" "}
                process. The interface stays the same, so a query that works locally is the one an
                agent uses later.
              </p>
            </div>
          </div>
        </div>
      </div>
    </section>
  );
}
