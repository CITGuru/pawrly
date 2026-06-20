import { SectionHeader } from "../UI";
import { CodeBlock } from "../CodeBlock";

const MCP = `pawrly mcp-stdio --config ./pawrly.yaml`;

const points = [
  {
    title: "One place to ask",
    body: "Instead of handing an agent a different tool for every system, give it approved tables it can query with SQL.",
  },
  {
    title: "Same names every time",
    body: "Sources are declared once. Every agent sees the same tables and columns, and every query can be logged, replayed, and reviewed.",
  },
  {
    title: "Works with MCP",
    body: "Connect Pawrly to Claude Desktop, Cursor, Codex, or other MCP clients, and let those clients query the same workspace.",
  },
];

export function Agents() {
  return (
    <section id="agents" className="relative scroll-mt-24 py-24 md:py-32">
      <div className="mx-auto max-w-6xl px-6">
        <div className="grid items-center gap-14 lg:grid-cols-2">
          <div className="flex flex-col gap-8">
            <SectionHeader
              align="left"
              eyebrow="For agents"
              title={
                <>
                  Stop making agents <span className="italic text-gold-2">write glue code.</span>
                </>
              }
              description="When an agent needs customer context, it should ask for the answer, not write a mini integration. Pawrly keeps the source setup in one place and gives the agent approved tables to query."
            />

            <div className="flex flex-col gap-4">
              {points.map((p) => (
                <div key={p.title} className="flex gap-4">
                  <span className="mt-1.5 h-2 w-2 shrink-0 rounded-full bg-gold" />
                  <div>
                    <h3 className="text-[15px] font-semibold text-cream">{p.title}</h3>
                    <p className="mt-1 text-sm leading-relaxed text-muted">{p.body}</p>
                  </div>
                </div>
              ))}
            </div>
          </div>

          <div className="flex flex-col gap-5">
            <CodeBlock lang="bash" title="connect any MCP client" code={MCP} />
            <div className="card rounded-2xl p-7">
              <p className="font-display text-2xl leading-snug text-cream">
                &ldquo;Find paying customers support hasn&apos;t heard from in a month.&rdquo;
              </p>
              <p className="mt-4 text-sm leading-relaxed text-muted">
                Pawrly turns that into a join across Stripe and Intercom. The agent asks for the
                customers, Pawrly handles the source details, and the query is there to review
                afterward.
              </p>
            </div>
          </div>
        </div>
      </div>
    </section>
  );
}
