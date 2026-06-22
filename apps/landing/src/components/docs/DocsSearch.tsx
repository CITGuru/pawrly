"use client";

import { useEffect, useRef, useState } from "react";
import { useRouter } from "next/navigation";

type Entry = { slug: string; doc: string; title: string; id: string; text: string };

function rank(entries: Entry[], q: string): Entry[] {
  const tokens = q.toLowerCase().trim().split(/\s+/).filter(Boolean);
  if (!tokens.length) return [];
  const scored: { e: Entry; score: number }[] = [];
  for (const e of entries) {
    const titleL = e.title.toLowerCase();
    const docL = e.doc.toLowerCase();
    const textL = e.text.toLowerCase();
    const hay = `${titleL} ${docL} ${textL}`;
    if (!tokens.every((t) => hay.includes(t))) continue;
    let score = 0;
    for (const t of tokens) {
      if (titleL.includes(t)) score += 4;
      if (docL.includes(t)) score += 1;
      if (textL.includes(t)) score += 1;
    }
    if (titleL.startsWith(tokens[0])) score += 3;
    scored.push({ e, score });
  }
  return scored.sort((a, b) => b.score - a.score).slice(0, 30).map((s) => s.e);
}

export function DocsSearch() {
  const router = useRouter();
  const [open, setOpen] = useState(false);
  const [q, setQ] = useState("");
  const [entries, setEntries] = useState<Entry[] | null>(null);
  const [active, setActive] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);

  // ⌘K / Ctrl+K to toggle
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setOpen((o) => !o);
      }
    }
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, []);

  // load index on first open, manage focus + scroll lock
  useEffect(() => {
    if (open) {
      if (entries === null) {
        fetch("/docs-search.json")
          .then((r) => r.json())
          .then(setEntries)
          .catch(() => setEntries([]));
      }
      const id = setTimeout(() => inputRef.current?.focus(), 0);
      document.body.style.overflow = "hidden";
      return () => {
        clearTimeout(id);
        document.body.style.overflow = "";
      };
    }
    setQ("");
    setActive(0);
  }, [open, entries]);

  const results = open ? rank(entries ?? [], q) : [];

  function go(e: Entry) {
    setOpen(false);
    router.push(`/docs/${e.slug}${e.id ? `#${e.id}` : ""}`);
  }

  function onKeyDown(e: React.KeyboardEvent) {
    if (e.key === "Escape") setOpen(false);
    else if (e.key === "ArrowDown") {
      e.preventDefault();
      setActive((a) => Math.min(results.length - 1, a + 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActive((a) => Math.max(0, a - 1));
    } else if (e.key === "Enter" && results[active]) {
      e.preventDefault();
      go(results[active]);
    }
  }

  return (
    <>
      <button
        type="button"
        onClick={() => setOpen(true)}
        aria-label="Search docs"
        className="inline-flex items-center gap-2 rounded-full border border-line bg-card px-3 py-2 text-sm text-muted-2 transition-colors hover:text-cream"
      >
        <svg width="15" height="15" viewBox="0 0 24 24" fill="none" aria-hidden>
          <circle cx="11" cy="11" r="7" stroke="currentColor" strokeWidth="1.8" />
          <path d="M20 20l-3.2-3.2" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" />
        </svg>
        <span className="hidden sm:inline">Search</span>
        <kbd className="hidden rounded border border-line px-1.5 py-0.5 font-mono text-[10px] sm:inline">
          ⌘K
        </kbd>
      </button>

      {open ? (
        <div className="fixed inset-0 z-[120] flex items-start justify-center px-4 pt-[12vh]">
          <div
            className="animate-fade-in absolute inset-0 bg-ocean-950/80 backdrop-blur-sm"
            onClick={() => setOpen(false)}
            aria-hidden
          />
          <div
            role="dialog"
            aria-modal="true"
            aria-label="Search documentation"
            className="animate-fade-up relative w-full max-w-xl overflow-hidden rounded-2xl border border-line-2 bg-ocean-850 soft-shadow-lg"
            onKeyDown={onKeyDown}
          >
            <div className="flex items-center gap-3 border-b border-line px-4">
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" aria-hidden className="text-muted-2">
                <circle cx="11" cy="11" r="7" stroke="currentColor" strokeWidth="1.8" />
                <path d="M20 20l-3.2-3.2" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" />
              </svg>
              <input
                ref={inputRef}
                value={q}
                onChange={(e) => {
                  setQ(e.target.value);
                  setActive(0);
                }}
                placeholder="Search the docs…"
                className="w-full bg-transparent py-4 text-sm text-cream outline-none placeholder:text-muted-2"
              />
              <kbd className="rounded border border-line px-1.5 py-0.5 font-mono text-[10px] text-muted-2">
                Esc
              </kbd>
            </div>

            <div className="max-h-[60vh] overflow-y-auto p-2">
              {q && results.length === 0 ? (
                <p className="px-3 py-8 text-center text-sm text-muted-2">No results for “{q}”.</p>
              ) : null}
              {!q ? (
                <p className="px-3 py-8 text-center text-sm text-muted-2">
                  Search guides and reference across the docs.
                </p>
              ) : null}
              {results.map((r, i) => (
                <button
                  key={`${r.slug}-${r.id}-${i}`}
                  type="button"
                  onClick={() => go(r)}
                  onMouseMove={() => setActive(i)}
                  className={`flex w-full flex-col gap-0.5 rounded-xl px-3 py-2.5 text-left transition-colors ${
                    i === active ? "bg-card-2" : "hover:bg-card"
                  }`}
                >
                  <span className="flex items-center gap-2">
                    <span className="text-sm font-medium text-cream">{r.title}</span>
                    <span className="font-mono text-[10px] uppercase tracking-[0.14em] text-gold/70">
                      {r.doc}
                    </span>
                  </span>
                  {r.text ? (
                    <span className="line-clamp-1 text-xs text-muted-2">{r.text}</span>
                  ) : null}
                </button>
              ))}
            </div>
          </div>
        </div>
      ) : null}
    </>
  );
}
