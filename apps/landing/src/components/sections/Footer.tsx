import Link from "next/link";
import { Logo } from "../Logo";
import { features } from "@/lib/features";

const cols: { heading: string; links: { label: string; href: string; external?: boolean }[] }[] = [
  {
    heading: "Product",
    links: [
      ...features.map((f) => ({ label: f.label, href: `/features/${f.slug}` })),
      { label: "Get started", href: "/#install" },
    ],
  },
  {
    heading: "Resources",
    links: [
      { label: "Docs", href: "https://github.com/CITGuru/pawrly#quickstart", external: true },
      { label: "Blog", href: "/blog" },
      { label: "GitHub", href: "https://github.com/CITGuru/pawrly", external: true },
      { label: "Examples", href: "https://github.com/CITGuru/pawrly/tree/main/examples", external: true },
    ],
  },
];

export function Footer() {
  return (
    <footer className="relative border-t border-line bg-ocean-900/50">
      <div className="mx-auto grid max-w-6xl gap-10 px-6 py-14 md:py-16 lg:grid-cols-12">
        <div className="flex flex-col gap-4 lg:col-span-5">
          <Logo className="text-lg" />
          <p className="max-w-xs text-sm leading-relaxed text-muted">
            Query APIs, files, MCP servers, and databases with SQL, then let agents use the
            same reviewed workspace.
          </p>
          <div className="mt-2 flex items-center gap-2">
            <Social href="https://github.com/CITGuru/pawrly" label="GitHub">
              <svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor">
                <path d="M8 .2a8 8 0 0 0-2.5 15.6c.4.1.5-.2.5-.4v-1.4c-2.2.5-2.7-1-2.7-1-.4-1-.9-1.2-.9-1.2-.7-.5.1-.5.1-.5.8.1 1.2.8 1.2.8.7 1.2 1.9.9 2.4.7.1-.5.3-.9.5-1.1-1.8-.2-3.6-.9-3.6-3.9 0-.9.3-1.6.8-2.2 0-.2-.4-1 .1-2.1 0 0 .7-.2 2.2.8a7.6 7.6 0 0 1 4 0c1.5-1 2.2-.8 2.2-.8.4 1.1 0 1.9.1 2.1.5.6.8 1.3.8 2.2 0 3-1.8 3.7-3.6 3.9.3.3.6.8.6 1.6v2.4c0 .2.1.5.5.4A8 8 0 0 0 8 .2Z" />
              </svg>
            </Social>
          </div>
        </div>

        <div className="grid grid-cols-2 gap-8 lg:col-span-7">
          {cols.map((c) => (
            <div key={c.heading} className="flex flex-col gap-3">
              <span className="text-xs uppercase tracking-[0.18em] text-muted-2">{c.heading}</span>
              <ul className="flex flex-col gap-2">
                {c.links.map((l) => (
                  <li key={l.label}>
                    {l.external ? (
                      <a
                        href={l.href}
                        target="_blank"
                        rel="noreferrer"
                        className="text-sm text-foam transition-colors hover:text-cream"
                      >
                        {l.label}
                      </a>
                    ) : (
                      <Link
                        href={l.href}
                        className="text-sm text-foam transition-colors hover:text-cream"
                      >
                        {l.label}
                      </Link>
                    )}
                  </li>
                ))}
              </ul>
            </div>
          ))}
        </div>
      </div>

      <div className="border-t border-line">
        <div className="mx-auto flex max-w-6xl flex-col items-center justify-between gap-3 px-6 py-5 text-xs text-muted-2 md:flex-row">
          <span>© {new Date().getFullYear()} Pawrly</span>
          <div className="flex items-center gap-5">
            <Link href="/privacy" className="transition-colors hover:text-cream">
              Privacy
            </Link>
            <Link href="/terms" className="transition-colors hover:text-cream">
              Terms
            </Link>
          </div>
        </div>
      </div>
    </footer>
  );
}

function Social({
  href,
  label,
  children,
}: {
  href: string;
  label: string;
  children: React.ReactNode;
}) {
  return (
    <a
      href={href}
      aria-label={label}
      target="_blank"
      rel="noreferrer"
      className="inline-flex h-9 w-9 items-center justify-center rounded-full border border-line text-foam transition-colors hover:text-cream"
    >
      {children}
    </a>
  );
}
