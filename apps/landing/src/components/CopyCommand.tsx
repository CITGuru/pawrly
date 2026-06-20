"use client";

import { useState } from "react";

/** A terminal-style install line with a one-click copy. */
export function CopyCommand({
  command,
  className = "",
}: {
  command: string;
  className?: string;
}) {
  const [copied, setCopied] = useState(false);

  async function copy() {
    try {
      await navigator.clipboard.writeText(command);
      setCopied(true);
      setTimeout(() => setCopied(false), 1600);
    } catch {
      // Clipboard can be unavailable (e.g. non-secure context) — fail quietly.
    }
  }

  return (
    <button
      type="button"
      onClick={copy}
      className={`group code-surface flex w-full items-center gap-3 rounded-full px-4 py-2.5 text-left soft-shadow ${className}`}
      aria-label="Copy install command"
    >
      <span className="select-none text-gold/80">$</span>
      <code className="flex-1 truncate font-mono text-[13px] text-cream">{command}</code>
      <span className="shrink-0 text-foam transition-colors group-hover:text-cream">
        {copied ? (
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" aria-hidden>
            <path
              d="M5 13l4 4L19 7"
              stroke="currentColor"
              strokeWidth="2"
              strokeLinecap="round"
              strokeLinejoin="round"
            />
          </svg>
        ) : (
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" aria-hidden>
            <rect x="9" y="9" width="11" height="11" rx="2.5" stroke="currentColor" strokeWidth="1.7" />
            <path
              d="M5 15V5a2 2 0 0 1 2-2h8"
              stroke="currentColor"
              strokeWidth="1.7"
              strokeLinecap="round"
            />
          </svg>
        )}
      </span>
    </button>
  );
}
