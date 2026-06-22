"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import { useEffect, useState } from "react";
import { Logo } from "../Logo";
import { ArrowRight } from "../UI";
import { DocsNav } from "./DocsNav";
import { DocsSearch } from "./DocsSearch";
import { docList } from "@/lib/docs-config";

const GITHUB = "https://github.com/CITGuru/pawrly";

// Docs-specific header: full-width sticky bar with a breadcrumb, and a hamburger
// that opens the global links + docs nav on mobile/tablet (below lg).
export function DocsHeader() {
  const pathname = usePathname();
  const [open, setOpen] = useState(false);
  const current = docList.find((d) => pathname === `/docs/${d.slug}`);

  // Close the drawer on navigation.
  useEffect(() => {
    setOpen(false);
  }, [pathname]);

  // Lock scroll + escape-to-close while the drawer is open.
  useEffect(() => {
    document.body.style.overflow = open ? "hidden" : "";
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") setOpen(false);
    }
    document.addEventListener("keydown", onKey);
    return () => {
      document.body.style.overflow = "";
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  return (
    <header className="sticky top-0 z-50 border-b border-line bg-ocean-950/80 backdrop-blur-xl">
      <div className="mx-auto flex h-16 max-w-7xl items-center gap-3 px-4 md:px-6">
        <Link href="/" className="flex items-center" aria-label="Pawrly home">
          <Logo />
        </Link>

        <span aria-hidden className="hidden h-5 w-px bg-line sm:block" />
        <nav aria-label="Breadcrumb" className="hidden items-center gap-1.5 text-sm sm:flex">
          <Link
            href="/docs"
            className={
              current ? "text-foam transition-colors hover:text-cream" : "font-medium text-cream"
            }
          >
            Docs
          </Link>
          {current ? (
            <>
              <span className="text-muted-2">/</span>
              <span className="font-medium text-cream">{current.title}</span>
            </>
          ) : null}
        </nav>

        <div className="ml-auto flex items-center gap-1.5">
          <DocsSearch />
          <Link
            href="/"
            className="hidden rounded-full px-3 py-2 text-sm text-foam transition-colors hover:text-cream lg:inline-flex"
          >
            Home
          </Link>
          <Link
            href="/blog"
            className="hidden rounded-full px-3 py-2 text-sm text-foam transition-colors hover:text-cream lg:inline-flex"
          >
            Blog
          </Link>
          <a
            href={GITHUB}
            target="_blank"
            rel="noreferrer"
            className="hidden items-center gap-2 rounded-full bg-sand px-4 py-2 text-sm font-semibold text-ocean-950 transition-colors hover:bg-gold-2 sm:inline-flex"
          >
            Star on GitHub
            <ArrowRight />
          </a>
          <button
            type="button"
            aria-label="Open menu"
            aria-expanded={open}
            onClick={() => setOpen((v) => !v)}
            className="inline-flex h-10 w-10 items-center justify-center rounded-full border border-line text-cream lg:hidden"
          >
            {open ? (
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none">
                <path d="M6 6l12 12M18 6L6 18" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" />
              </svg>
            ) : (
              <svg width="16" height="12" viewBox="0 0 16 12" fill="none">
                <path d="M1 1h14M1 6h14M1 11h14" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
              </svg>
            )}
          </button>
        </div>
      </div>

      {/* Mobile / tablet drawer */}
      {open ? (
        <div className="lg:hidden">
          <div
            className="fixed inset-0 top-16 z-40 bg-ocean-950/60 backdrop-blur-[2px]"
            onClick={() => setOpen(false)}
            aria-hidden
          />
          <div className="animate-fade-up absolute inset-x-0 top-full z-50 max-h-[calc(100dvh-4rem)] overflow-y-auto border-b border-line bg-ocean-950/95 px-4 py-5 backdrop-blur-xl md:px-6">
            <div className="mb-5 flex flex-col gap-1">
              <Link href="/" className="rounded-lg px-3 py-2 text-sm text-foam hover:bg-card hover:text-cream">
                Home
              </Link>
              <Link href="/blog" className="rounded-lg px-3 py-2 text-sm text-foam hover:bg-card hover:text-cream">
                Blog
              </Link>
              <a
                href={GITHUB}
                target="_blank"
                rel="noreferrer"
                className="rounded-lg px-3 py-2 text-sm text-foam hover:bg-card hover:text-cream"
              >
                GitHub
              </a>
            </div>
            <div className="border-t border-line pt-5">
              <DocsNav onNavigate={() => setOpen(false)} />
            </div>
          </div>
        </div>
      ) : null}
    </header>
  );
}
