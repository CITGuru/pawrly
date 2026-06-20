"use client";

import Link from "next/link";
import { useEffect, useState } from "react";
import { Logo } from "./Logo";
import { ArrowRight } from "./UI";

const GITHUB = "https://github.com/CITGuru/pawrly";

const links = [
  { label: "How it works", href: "/#how" },
  { label: "Sources", href: "/#sources" },
  { label: "For agents", href: "/#agents" },
  { label: "Blog", href: "/#blog" },
];

export function Nav() {
  const [open, setOpen] = useState(false);

  useEffect(() => {
    document.body.style.overflow = open ? "hidden" : "";
    return () => {
      document.body.style.overflow = "";
    };
  }, [open]);

  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") setOpen(false);
    }
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, []);

  return (
    <header className="fixed inset-x-0 top-4 z-50 flex justify-center px-4">
      {open ? (
        <div
          className="animate-fade-in fixed inset-0 -z-10 bg-ocean-950/60 backdrop-blur-[2px]"
          onClick={() => setOpen(false)}
          aria-hidden
        />
      ) : null}

      <div className="relative w-full max-w-5xl">
        <nav
          aria-label="Primary"
          className="glass flex items-center gap-2 rounded-full border border-line px-3 py-2 soft-shadow"
        >
          <Link href="/" className="flex items-center px-2 py-1" onClick={() => setOpen(false)}>
            <Logo />
          </Link>

          <ul className="ml-3 hidden items-center gap-0.5 md:flex">
            {links.map((l) => (
              <li key={l.label}>
                <Link
                  href={l.href}
                  className="rounded-full px-3 py-2 text-sm text-foam transition-colors hover:text-cream"
                >
                  {l.label}
                </Link>
              </li>
            ))}
          </ul>

          <div className="ml-auto flex items-center gap-2">
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
              className="inline-flex h-10 w-10 items-center justify-center rounded-full border border-line text-cream md:hidden"
            >
              <svg width="16" height="12" viewBox="0 0 16 12" fill="none">
                <path
                  d="M1 1h14M1 6h14M1 11h14"
                  stroke="currentColor"
                  strokeWidth="1.6"
                  strokeLinecap="round"
                />
              </svg>
            </button>
          </div>
        </nav>

        {open ? (
          <div className="animate-fade-up absolute inset-x-0 top-full pt-2.5 md:hidden">
            <div className="glass rounded-3xl border border-line p-3 soft-shadow-lg">
              <ul className="flex flex-col gap-0.5">
                {links.map((l) => (
                  <li key={l.label}>
                    <Link
                      href={l.href}
                      onClick={() => setOpen(false)}
                      className="block rounded-2xl px-4 py-3 text-base text-cream hover:bg-card-2"
                    >
                      {l.label}
                    </Link>
                  </li>
                ))}
                <li className="mt-2 px-2">
                  <a
                    href={GITHUB}
                    target="_blank"
                    rel="noreferrer"
                    onClick={() => setOpen(false)}
                    className="flex items-center justify-center gap-2 rounded-full bg-sand px-4 py-3 text-sm font-semibold text-ocean-950"
                  >
                    Star on GitHub
                    <ArrowRight />
                  </a>
                </li>
              </ul>
            </div>
          </div>
        ) : null}
      </div>
    </header>
  );
}
