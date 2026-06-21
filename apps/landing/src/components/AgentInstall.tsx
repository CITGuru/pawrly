"use client";

import { useEffect, useRef, useState } from "react";
import { AGENTS, type AgentId, buildAgentPrompt } from "@/lib/agent-prompt";

/**
 * Split CTA: the main button copies a ready-to-paste "install & set up Pawrly"
 * prompt for any agent; the caret opens a menu to copy a prompt tailored to a
 * specific agent. Paste it into your coding agent and it does the setup.
 */
export function AgentInstall() {
  const [open, setOpen] = useState(false);
  const [copied, setCopied] = useState<string | null>(null);
  const ref = useRef<HTMLDivElement>(null);

  async function copyFor(id: AgentId, label: string) {
    try {
      await navigator.clipboard.writeText(buildAgentPrompt(id));
      setCopied(label);
      setTimeout(() => setCopied(null), 2400);
    } catch {
      // Clipboard unavailable (non-secure context) — fail quietly.
    }
    setOpen(false);
  }

  useEffect(() => {
    if (!open) return;
    function onDoc(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") setOpen(false);
    }
    document.addEventListener("mousedown", onDoc);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDoc);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  return (
    <div ref={ref} className="relative">
      <div className="inline-flex items-stretch overflow-hidden rounded-full border border-line-2 bg-card">
        <button
          type="button"
          onClick={() => copyFor("any", "your agent")}
          className="inline-flex items-center gap-2 px-5 py-3 text-sm font-medium text-cream transition-colors hover:bg-card-2"
        >
          {copied ? <CheckIcon /> : <SparkIcon />}
          {copied ? `Copied — paste into ${copied}` : "Set up with your agent"}
        </button>
        <button
          type="button"
          aria-label="Choose a specific agent"
          aria-expanded={open}
          onClick={() => setOpen((v) => !v)}
          className="inline-flex items-center border-l border-line px-3 text-foam transition-colors hover:bg-card-2 hover:text-cream"
        >
          <Caret open={open} />
        </button>
      </div>

      {open ? (
        <div className="glass animate-fade-up absolute left-1/2 top-full z-50 mt-2 w-64 -translate-x-1/2 rounded-2xl border border-line p-2 soft-shadow-lg">
          <p className="px-3 pb-1 pt-2 text-left text-[10px] font-medium uppercase tracking-[0.18em] text-muted-2">
            Copy a setup prompt for…
          </p>
          {AGENTS.map((a) => (
            <button
              key={a.id}
              type="button"
              onClick={() => copyFor(a.id, a.label)}
              className="group flex w-full items-center justify-between gap-2 rounded-xl px-3 py-2 text-left text-sm text-cream transition-colors hover:bg-card-2"
            >
              {a.label}
              <span className="text-muted-2 transition-colors group-hover:text-gold">
                <CopyGlyph />
              </span>
            </button>
          ))}
        </div>
      ) : null}
    </div>
  );
}

function SparkIcon() {
  return (
    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" aria-hidden>
      <path
        d="M12 3l1.6 4.4L18 9l-4.4 1.6L12 15l-1.6-4.4L6 9l4.4-1.6L12 3Z"
        fill="currentColor"
      />
      <path d="M18.5 14l.8 2.2 2.2.8-2.2.8-.8 2.2-.8-2.2-2.2-.8 2.2-.8.8-2.2Z" fill="currentColor" />
    </svg>
  );
}

function CheckIcon() {
  return (
    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" aria-hidden>
      <path
        d="M5 13l4 4L19 7"
        stroke="currentColor"
        strokeWidth="2.2"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function CopyGlyph() {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden>
      <rect x="9" y="9" width="11" height="11" rx="2.5" stroke="currentColor" strokeWidth="1.7" />
      <path d="M5 15V5a2 2 0 0 1 2-2h8" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" />
    </svg>
  );
}

function Caret({ open }: { open: boolean }) {
  return (
    <svg
      width="12"
      height="12"
      viewBox="0 0 24 24"
      fill="none"
      aria-hidden
      className={`transition-transform duration-200 ${open ? "rotate-180" : ""}`}
    >
      <path d="M6 9l6 6 6-6" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}
