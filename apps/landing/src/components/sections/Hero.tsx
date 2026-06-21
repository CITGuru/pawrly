import Link from "next/link";
import { ArrowRight } from "../UI";
import { CopyCommand } from "../CopyCommand";
import { CodeBlock } from "../CodeBlock";
import { AgentInstall } from "../AgentInstall";

const INSTALL = "curl -fsSL https://pawrly.dev/install.sh | sh";

const HERO_SQL = `-- two live APIs, joined in one statement
SELECT c.email,c.name, i.last_seen_at
FROM stripe.customers c
JOIN intercom.contacts i ON i.email = c.email
WHERE c.delinquent = false 
ORDER BY i.last_seen_at ASC`;

export function Hero() {
  return (
    <section className="bg-water relative overflow-hidden pt-36 pb-20 md:pt-44 md:pb-28">
      {/* Wandering sun-glints over the water */}
      <div
        aria-hidden
        className="animate-caustics pointer-events-none absolute inset-0 opacity-70"
        style={{
          background:
            "radial-gradient(38% 26% at 50% 4%, rgba(241,220,176,0.28), transparent 70%), radial-gradient(30% 22% at 78% 20%, rgba(231,195,137,0.20), transparent 72%), radial-gradient(34% 24% at 20% 30%, rgba(150,200,235,0.16), transparent 70%)",
        }}
      />
      {/* Fade the water into the deep below the fold */}
      <div
        aria-hidden
        className="pointer-events-none absolute inset-x-0 bottom-0 h-40"
        style={{ background: "linear-gradient(180deg, transparent, var(--ocean-950))" }}
      />

      <div className="relative mx-auto flex max-w-6xl flex-col items-center gap-7 px-6 text-center">
        <Link
          href="/blog/agents-need-a-query-surface-not-more-tools"
          className="glass inline-flex items-center gap-2 rounded-full border border-line px-3 py-1.5 text-xs text-foam transition-colors hover:text-cream"
        >
          <span className="rounded-full bg-gold px-2 py-0.5 text-[10px] font-semibold uppercase tracking-wide text-ocean-950">
            New
          </span>
          Agents need a query surface, not more tools
          <ArrowRight />
        </Link>

        <h1 className="font-display max-w-[16ch] text-5xl leading-[1.04] tracking-tight text-cream sm:text-6xl md:text-7xl lg:text-[82px]">
          One SQL interface for{" "}
          <span className="italic text-gold-2">APIs.</span>
        </h1>

        <p className="max-w-2xl text-lg leading-relaxed text-muted md:text-xl">
          Connect the APIs, files, MCPs and databases your team already uses. Pawrly gives them
          table names, columns, and joins, so people and agents can ask one SQL question
          instead of stitching together data from multiple sources.
        </p>

        <div className="mt-1 w-full max-w-md">
          <CopyCommand command={INSTALL} />
        </div>

        <div className="mt-1 flex flex-col items-center gap-3 sm:flex-row">
          <AgentInstall />
          <a
            href="https://github.com/CITGuru/pawrly#quickstart"
            target="_blank"
            rel="noreferrer"
            className="inline-flex items-center gap-2 rounded-full bg-sand px-5 py-3 text-sm font-semibold text-ocean-950 transition-colors hover:bg-gold-2"
          >
            Read the docs
            <ArrowRight />
          </a>
        </div>

        <p className="mt-2 text-xs tracking-wide text-muted-2">
         Local SQL runtime over your APIs, files and MCPs for Agents
        </p>

        {/* The query, sitting on the water */}
        {/* <div className="mt-8 w-full max-w-2xl text-left md:mt-10">
          <CodeBlock lang="sql" title="pawrly sql" code={HERO_SQL} />
        </div> */}
      </div>
    </section>
  );
}
