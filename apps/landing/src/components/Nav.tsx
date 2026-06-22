"use client";

import Link from "next/link";
import { useEffect, useRef, useState } from "react";
import { Logo } from "./Logo";
import { ArrowRight } from "./UI";
import { features } from "@/lib/features";

const GITHUB = "https://github.com/CITGuru/pawrly";
type NavLink = { label: string; href?: string; external?: boolean; menu?: "features" };

const links: NavLink[] = [
  { label: "Features", menu: "features" },
  { label: "Docs", href: "/docs" },
  { label: "Blog", href: "/blog" },
];

export function Nav() {
  const [mobileOpen, setMobileOpen] = useState(false);
  const [mobileFeatures, setMobileFeatures] = useState(false);
  const [menu, setMenu] = useState<"features" | null>(null);
  const navRef = useRef<HTMLDivElement>(null);
  const closeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  function clearCloseTimer() {
    if (closeTimer.current) {
      clearTimeout(closeTimer.current);
      closeTimer.current = null;
    }
  }
  function openMenu() {
    clearCloseTimer();
    setMenu("features");
  }
  function scheduleClose() {
    clearCloseTimer();
    closeTimer.current = setTimeout(() => setMenu(null), 140);
  }
  function closeAll() {
    clearCloseTimer();
    setMenu(null);
    setMobileOpen(false);
    setMobileFeatures(false);
  }

  useEffect(() => () => clearCloseTimer(), []);

  useEffect(() => {
    document.body.style.overflow = mobileOpen ? "hidden" : "";
    return () => {
      document.body.style.overflow = "";
    };
  }, [mobileOpen]);

  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") closeAll();
    }
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, []);

  useEffect(() => {
    if (!menu) return;
    function onClick(e: MouseEvent) {
      if (navRef.current && !navRef.current.contains(e.target as Node)) setMenu(null);
    }
    document.addEventListener("mousedown", onClick);
    return () => document.removeEventListener("mousedown", onClick);
  }, [menu]);

  return (
    <header className="fixed inset-x-0 top-4 z-50 flex justify-center px-4">
      {menu || mobileOpen ? (
        <div
          className="animate-fade-in fixed inset-0 -z-10 bg-ocean-950/60 backdrop-blur-[2px]"
          onClick={closeAll}
          aria-hidden
        />
      ) : null}

      <div ref={navRef} className="relative w-full max-w-5xl">
        <nav
          aria-label="Primary"
          className="glass flex items-center gap-2 rounded-full border border-line px-3 py-2 soft-shadow"
        >
          <Link href="/" className="flex items-center px-2 py-1" onClick={closeAll}>
            <Logo />
          </Link>

          <ul className="ml-3 hidden items-center gap-0.5 md:flex">
            {links.map((l) =>
              l.menu ? (
                <li key={l.label}>
                  <button
                    type="button"
                    aria-haspopup="true"
                    aria-expanded={menu === "features"}
                    onClick={() => setMenu((cur) => (cur ? null : "features"))}
                    onMouseEnter={openMenu}
                    onMouseLeave={scheduleClose}
                    className={`inline-flex items-center gap-1 rounded-full px-3 py-2 text-sm transition-colors ${
                      menu === "features" ? "text-cream" : "text-foam hover:text-cream"
                    }`}
                  >
                    {l.label}
                    <Chevron open={menu === "features"} />
                  </button>
                </li>
              ) : l.external ? (
                <li key={l.label}>
                  <a
                    href={l.href}
                    target="_blank"
                    rel="noreferrer"
                    className="rounded-full px-3 py-2 text-sm text-foam transition-colors hover:text-cream"
                  >
                    {l.label}
                  </a>
                </li>
              ) : (
                <li key={l.label}>
                  <Link
                    href={l.href!}
                    onClick={closeAll}
                    className="rounded-full px-3 py-2 text-sm text-foam transition-colors hover:text-cream"
                  >
                    {l.label}
                  </Link>
                </li>
              )
            )}
          </ul>

          <div className="ml-auto flex items-center gap-2">
            <a
              href={GITHUB}
              target="_blank"
              rel="noreferrer"
              onClick={closeAll}
              className="hidden items-center gap-2 rounded-full bg-sand px-4 py-2 text-sm font-semibold text-ocean-950 transition-colors hover:bg-gold-2 sm:inline-flex"
            >
              Star on GitHub
              <ArrowRight />
            </a>
            <button
              type="button"
              aria-label="Open menu"
              aria-expanded={mobileOpen}
              onClick={() => setMobileOpen((v) => !v)}
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

        {/* Desktop Features dropdown */}
        {menu === "features" ? (
          <div
            role="menu"
            aria-label="Features"
            onMouseEnter={openMenu}
            onMouseLeave={scheduleClose}
            className="animate-fade-up absolute left-0 top-full hidden w-80 pt-2.5 md:block"
          >
            <div className="glass rounded-3xl border border-line p-2 soft-shadow-lg">
              {features.map((f) => (
                <Link
                  key={f.slug}
                  href={`/features/${f.slug}`}
                  role="menuitem"
                  onClick={closeAll}
                  className="flex flex-col gap-0.5 rounded-2xl px-3 py-3 transition-colors hover:bg-card-2"
                >
                  <span className="text-sm font-medium text-cream">{f.label}</span>
                  <span className="text-xs leading-snug text-muted">{f.tagline}</span>
                </Link>
              ))}
            </div>
          </div>
        ) : null}
      </div>

      {/* Mobile menu */}
      {mobileOpen ? (
        <div className="animate-fade-up absolute inset-x-0 top-full px-4 md:hidden">
          <div className="glass mx-auto max-w-5xl rounded-3xl border border-line p-3 soft-shadow-lg">
            <ul className="flex flex-col gap-0.5">
              <li className="flex flex-col">
                <button
                  type="button"
                  aria-expanded={mobileFeatures}
                  onClick={() => setMobileFeatures((v) => !v)}
                  className="flex items-center justify-between rounded-2xl px-4 py-3 text-base text-cream hover:bg-card-2"
                >
                  Features
                  <Chevron open={mobileFeatures} />
                </button>
                {mobileFeatures ? (
                  <div className="flex flex-col pb-1 pl-2">
                    {features.map((f) => (
                      <Link
                        key={f.slug}
                        href={`/features/${f.slug}`}
                        onClick={closeAll}
                        className="flex flex-col gap-0.5 rounded-xl px-3 py-2.5 hover:bg-card-2"
                      >
                        <span className="text-sm font-medium text-cream">{f.label}</span>
                        <span className="text-xs leading-snug text-muted">{f.tagline}</span>
                      </Link>
                    ))}
                  </div>
                ) : null}
              </li>
              <li>
                <Link
                  href="/docs"
                  onClick={closeAll}
                  className="block rounded-2xl px-4 py-3 text-base text-cream hover:bg-card-2"
                >
                  Docs
                </Link>
              </li>
              <li>
                <Link
                  href="/blog"
                  onClick={closeAll}
                  className="block rounded-2xl px-4 py-3 text-base text-cream hover:bg-card-2"
                >
                  Blog
                </Link>
              </li>
              <li className="mt-2 px-2">
                <a
                  href={GITHUB}
                  target="_blank"
                  rel="noreferrer"
                  onClick={closeAll}
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
    </header>
  );
}

function Chevron({ open }: { open: boolean }) {
  return (
    <svg
      width="11"
      height="11"
      viewBox="0 0 24 24"
      fill="none"
      aria-hidden
      className={`transition-transform duration-200 ${open ? "rotate-180" : ""}`}
    >
      <path
        d="M6 9l6 6 6-6"
        stroke="currentColor"
        strokeWidth="2.2"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}
