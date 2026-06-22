import { ReactNode } from "react";
import { DocsHeader } from "./DocsHeader";
import { Grain } from "../Grain";
import { Footer } from "../sections/Footer";
import { DocsSidebar } from "./DocsSidebar";
import { DocsToc } from "./DocsToc";
import type { TocEntry } from "@/lib/docs-config";

/** 3-column docs chrome: sidebar + content (+ optional right-rail TOC). */
export function DocsShell({
  children,
  toc,
}: {
  children: ReactNode;
  toc?: TocEntry[];
}) {
  return (
    <>
      <Grain />
      <DocsHeader />
      <div className="mx-auto w-full max-w-7xl px-4 pt-8 md:px-6 md:pt-10">
        <div
          className={`lg:grid lg:gap-10 ${
            toc
              ? "lg:grid-cols-[14rem_minmax(0,1fr)] xl:grid-cols-[14rem_minmax(0,1fr)_13rem]"
              : "lg:grid-cols-[14rem_minmax(0,1fr)]"
          }`}
        >
          <DocsSidebar />
          <main className="min-w-0 pb-24">{children}</main>
          {toc ? <DocsToc toc={toc} /> : null}
        </div>
      </div>
      <Footer />
    </>
  );
}
