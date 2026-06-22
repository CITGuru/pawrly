"use client";

import { useEffect } from "react";
import { usePathname } from "next/navigation";

const COPY_SVG = `<svg width="14" height="14" viewBox="0 0 24 24" fill="none"><rect x="9" y="9" width="11" height="11" rx="2.5" stroke="currentColor" stroke-width="1.7"/><path d="M5 15V5a2 2 0 0 1 2-2h8" stroke="currentColor" stroke-width="1.7" stroke-linecap="round"/></svg>`;
const CHECK_SVG = `<svg width="14" height="14" viewBox="0 0 24 24" fill="none"><path d="M5 13l4 4L19 7" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"/></svg>`;

// Effect-only island: adds a copy button to each rendered code block. The
// markdown HTML is server-rendered, so we enhance it after hydration.
export function CodeCopyEnhancer() {
  const pathname = usePathname();
  useEffect(() => {
    const blocks = document.querySelectorAll<HTMLElement>(".doc-prose .code-block");
    blocks.forEach((block) => {
      if (block.querySelector(".code-copy")) return;
      const pre = block.querySelector("pre");
      if (!pre) return;
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "code-copy";
      btn.setAttribute("aria-label", "Copy code");
      btn.innerHTML = COPY_SVG;
      btn.addEventListener("click", async () => {
        try {
          await navigator.clipboard.writeText(pre.innerText.replace(/\n$/, ""));
          btn.innerHTML = CHECK_SVG;
          setTimeout(() => {
            btn.innerHTML = COPY_SVG;
          }, 1600);
        } catch {
          // ignore
        }
      });
      block.appendChild(btn);
    });
  }, [pathname]);

  return null;
}
