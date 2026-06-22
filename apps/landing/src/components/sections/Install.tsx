import { Mark } from "../Mark";
import { GhostCTA, ArrowRight } from "../UI";
import { CopyCommand } from "../CopyCommand";

const INSTALL = "curl -fsSL https://pawrly.dev/install.sh | sh";

export function Install() {
  return (
    <section id="install" className="relative scroll-mt-24 px-6 py-24 md:py-32">
      <div className="bg-water relative mx-auto max-w-5xl overflow-hidden rounded-[32px] border border-line px-6 py-16 text-center soft-shadow-lg md:px-12 md:py-20">
        <div
          aria-hidden
          className="animate-caustics pointer-events-none absolute inset-0 opacity-60"
          style={{
            background:
              "radial-gradient(40% 40% at 50% 0%, rgba(241,220,176,0.22), transparent 70%), radial-gradient(30% 30% at 80% 30%, rgba(150,200,235,0.16), transparent 72%)",
          }}
        />
        <div className="relative mx-auto flex max-w-2xl flex-col items-center gap-6">
          <Mark size={44} className="text-cream" carve="var(--ocean-800)" />
          <h2 className="font-display text-4xl leading-[1.06] tracking-tight text-cream md:text-5xl">
            One binary. One config. <span className="italic text-gold-2">Every source.</span>
          </h2>
          <p className="max-w-xl text-base leading-relaxed text-muted md:text-lg">
            Start with one config file, then query APIs, files, MCP servers, and databases from
            the same command. Add MCP when you want agents to use that same workspace.
          </p>

          <div className="mt-2 w-full max-w-md">
            <CopyCommand command={INSTALL} />
          </div>

          <div className="mt-2 flex flex-col items-center gap-3 sm:flex-row">
            <a
              href="https://github.com/CITGuru/pawrly"
              target="_blank"
              rel="noreferrer"
              className="inline-flex items-center gap-2 rounded-full bg-sand px-5 py-3 text-sm font-semibold text-ocean-950 transition-colors hover:bg-gold-2"
            >
              Star on GitHub
              <ArrowRight />
            </a>
            <GhostCTA href="/docs">Read the docs</GhostCTA>
          </div>
        </div>
      </div>
    </section>
  );
}
