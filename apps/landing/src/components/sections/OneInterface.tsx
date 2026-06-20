import { SectionHeader } from "../UI";
import { CodeBlock } from "../CodeBlock";

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
            - { name: email,   type: varchar }
            - { name: balance, type: bigint }`;

const SQL = `pawrly sql "
  SELECT email, balance
  FROM stripe.customers
  WHERE delinquent = true
"`;

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

        <div className="mt-14 grid items-start gap-5 lg:grid-cols-[1.15fr_1fr]">
          <CodeBlock lang="yaml" title="pawrly.yaml" code={YAML} />

          <div className="flex flex-col gap-5">
            <div className="card rounded-2xl p-6">
              <p className="text-sm leading-relaxed text-muted">
                The vendor already wrote the contract. Point an{" "}
                <code className="rounded bg-white/5 px-1.5 py-0.5 font-mono text-[12px] text-gold-2">
                  http
                </code>{" "}
                source at an OpenAPI 3.0 spec and Pawrly synthesizes one typed table per{" "}
                <code className="rounded bg-white/5 px-1.5 py-0.5 font-mono text-[12px] text-gold-2">
                  GET
                </code>{" "}
                — no hand-written schema needed.
              </p>
            </div>

            <CodeBlock lang="bash" title="now it's a table" code={SQL} />

            <div className="card rounded-2xl p-6">
              <p className="text-sm leading-relaxed text-muted">
                Run the query from the CLI, an MCP client, or a long-running{" "}
                <code className="rounded bg-white/5 px-1.5 py-0.5 font-mono text-[12px] text-gold-2">
                  pawrly serve
                </code>{" "}
                process. The interface stays the same, so a query that works locally is the same
                query an agent can use later.
              </p>
            </div>
          </div>
        </div>
      </div>
    </section>
  );
}
