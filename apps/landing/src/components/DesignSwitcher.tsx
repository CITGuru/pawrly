"use client";

import { useEffect, useState } from "react";

// TEMPORARY: a live A/B switcher for deciding the palette + mark direction.
// Flip the toggles and the whole site re-themes via <html data-theme/data-mark>;
// choices persist in localStorage. Shareable via ?theme=warm&mark=palm.
// Delete this component (and its mount in layout.tsx) once a direction is locked.

type Theme = "cool" | "warm";
type MarkKind = "paw" | "palm";

const ENABLED =
  process.env.NODE_ENV !== "production" ||
  process.env.NEXT_PUBLIC_DESIGN_SWITCHER === "1";

function apply(theme: Theme, mark: MarkKind) {
  const el = document.documentElement;
  if (theme === "warm") el.setAttribute("data-theme", "warm");
  else el.removeAttribute("data-theme");
  if (mark === "palm") el.setAttribute("data-mark", "palm");
  else el.removeAttribute("data-mark");
}

export function DesignSwitcher() {
  const [theme, setTheme] = useState<Theme>("cool");
  const [mark, setMark] = useState<MarkKind>("paw");
  const [open, setOpen] = useState(true);

  // Hydrate from URL params first, then localStorage.
  useEffect(() => {
    const p = new URLSearchParams(window.location.search);
    const t = (p.get("theme") || localStorage.getItem("pawrly:theme")) as Theme | null;
    const m = (p.get("mark") || localStorage.getItem("pawrly:mark")) as MarkKind | null;
    const nextTheme: Theme = t === "warm" ? "warm" : "cool";
    const nextMark: MarkKind = m === "palm" ? "palm" : "paw";
    setTheme(nextTheme);
    setMark(nextMark);
    apply(nextTheme, nextMark);
  }, []);

  function choose(nextTheme: Theme, nextMark: MarkKind) {
    setTheme(nextTheme);
    setMark(nextMark);
    apply(nextTheme, nextMark);
    localStorage.setItem("pawrly:theme", nextTheme);
    localStorage.setItem("pawrly:mark", nextMark);
  }

  if (!ENABLED) return null;

  return (
    <div className="fixed bottom-4 right-4 z-[200] font-sans">
      {open ? (
        <div className="glass w-60 rounded-2xl border border-line p-4 soft-shadow-lg">
          <div className="mb-3 flex items-center justify-between">
            <span className="text-[11px] font-medium uppercase tracking-[0.18em] text-gold/90">
              Preview
            </span>
            <button
              type="button"
              onClick={() => setOpen(false)}
              aria-label="Collapse"
              className="text-foam transition-colors hover:text-cream"
            >
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden>
                <path d="M6 9l6 6 6-6" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round" />
              </svg>
            </button>
          </div>

          <Row label="Palette">
            <Seg active={theme === "cool"} onClick={() => choose("cool", mark)}>Cool</Seg>
            <Seg active={theme === "warm"} onClick={() => choose("warm", mark)}>Warm</Seg>
          </Row>

          <Row label="Mark">
            <Seg active={mark === "paw"} onClick={() => choose(theme, "paw")}>Paw</Seg>
            <Seg active={mark === "palm"} onClick={() => choose(theme, "palm")}>Palm</Seg>
          </Row>

          <p className="mt-3 text-[10px] leading-snug text-muted-2">
            Live preview only. Social &amp; app icons regenerate from your final pick.
          </p>
        </div>
      ) : (
        <button
          type="button"
          onClick={() => setOpen(true)}
          className="glass rounded-full border border-line px-4 py-2 text-xs font-medium text-cream soft-shadow"
        >
          Preview
        </button>
      )}
    </div>
  );
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="mb-2.5 flex items-center justify-between gap-2">
      <span className="text-xs text-muted">{label}</span>
      <div className="flex gap-1 rounded-full border border-line bg-card p-0.5">{children}</div>
    </div>
  );
}

function Seg({
  active,
  onClick,
  children,
}: {
  active: boolean;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`rounded-full px-3 py-1 text-xs font-medium transition-colors ${
        active ? "bg-sand text-ocean-950" : "text-foam hover:text-cream"
      }`}
    >
      {children}
    </button>
  );
}
