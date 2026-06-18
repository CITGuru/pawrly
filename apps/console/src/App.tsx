import { useCallback, useEffect, useMemo, useState } from "react";

import { ConnectionProvider } from "@/lib/connection";
import { NavContext, type NavValue } from "@/lib/nav";
import { Sidebar, PAGE_LABELS, type PageId } from "@/components/Sidebar";
import { Topbar } from "@/components/Topbar";
import { SourcesPanel } from "@/components/panels/SourcesPanel";
import { CatalogPanel } from "@/components/panels/CatalogPanel";
import { SemanticPanel } from "@/components/panels/SemanticPanel";
import { CacheEntriesPanel } from "@/components/panels/CacheEntriesPanel";
import { ActivityPanel } from "@/components/panels/ActivityPanel";
import { SqlRunner } from "@/components/panels/SqlRunner";

const COLLAPSE_KEY = "pawrly.console.sidebarCollapsed";
const DEFAULT_PAGE: PageId = "sources";

/** Read the active page from `?page=` in the URL, falling back to the default. */
function pageFromUrl(): PageId {
  const p = new URLSearchParams(window.location.search).get("page");
  return p && Object.prototype.hasOwnProperty.call(PAGE_LABELS, p)
    ? (p as PageId)
    : DEFAULT_PAGE;
}

/** Reflect the active page into `?page=` so refresh / back / forward work. */
function writePageToUrl(page: PageId, replace: boolean) {
  const url = new URL(window.location.href);
  url.searchParams.set("page", page);
  const state = { page };
  if (replace) window.history.replaceState(state, "", url);
  else window.history.pushState(state, "", url);
}

function Page({ page }: { page: PageId }) {
  switch (page) {
    case "sources":
      return <SourcesPanel />;
    case "catalog":
      return <CatalogPanel />;
    case "semantic":
      return <SemanticPanel />;
    case "cache":
      return <CacheEntriesPanel materialized={false} />;
    case "materialized":
      return <CacheEntriesPanel materialized={true} />;
    case "activity":
      return <ActivityPanel />;
    case "sql":
      return <SqlRunner />;
  }
}

function Shell() {
  const [page, setPageState] = useState<PageId>(pageFromUrl);
  const [pendingSql, setPendingSql] = useState<string | null>(null);
  const [collapsed, setCollapsed] = useState(
    () => localStorage.getItem(COLLAPSE_KEY) === "1",
  );

  // Navigate + sync the URL. Pushes a history entry only when the page actually
  // changes (so clicking the current item doesn't pile up entries).
  const setPage = useCallback((next: PageId) => {
    if (pageFromUrl() !== next) writePageToUrl(next, false);
    setPageState(next);
  }, []);

  // Canonicalize the URL on first load (e.g. landed on `/` with no `?page`),
  // and follow browser back/forward.
  useEffect(() => {
    writePageToUrl(pageFromUrl(), true);
    const onPop = () => setPageState(pageFromUrl());
    window.addEventListener("popstate", onPop);
    return () => window.removeEventListener("popstate", onPop);
  }, []);

  function toggleCollapsed() {
    setCollapsed((c) => {
      const next = !c;
      localStorage.setItem(COLLAPSE_KEY, next ? "1" : "0");
      return next;
    });
  }

  const nav = useMemo<NavValue>(
    () => ({
      navigate: setPage,
      openSql: (sql: string) => {
        setPendingSql(sql);
        setPage("sql");
      },
      pendingSql,
      consumePendingSql: () => setPendingSql(null),
    }),
    [pendingSql],
  );

  return (
    <NavContext.Provider value={nav}>
      <div className="bg-background text-foreground flex h-screen overflow-hidden">
        <Sidebar active={page} onNavigate={setPage} collapsed={collapsed} />
        <div className="flex flex-1 flex-col overflow-hidden">
          <Topbar page={page} collapsed={collapsed} onToggle={toggleCollapsed} />
          <main className="flex-1 overflow-y-auto">
            <div className="mx-auto max-w-7xl px-8 py-8">
              <Page page={page} />
            </div>
          </main>
        </div>
      </div>
    </NavContext.Provider>
  );
}

export default function App() {
  return (
    <ConnectionProvider>
      <Shell />
    </ConnectionProvider>
  );
}
