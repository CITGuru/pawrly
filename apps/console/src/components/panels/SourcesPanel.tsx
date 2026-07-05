import { useCallback, useEffect, useMemo, useState, type ReactNode } from "react";
import {
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  ChevronsLeft,
  ChevronsRight,
  ChevronUp,
  MoreHorizontal,
  Plus,
  Search,
} from "lucide-react";

import { cn } from "@/lib/utils";
import { useClients } from "@/lib/connection";
import { useNav } from "@/lib/nav";
import { useAsync } from "@/lib/useAsync";
import { errMsg, formatTimestamp, sourceKindLabel } from "@/lib/format";
import { loadSourceUsage, type SourceUsage } from "@/lib/activityUsage";
import {
  connectorForSource,
  DATABASE_TAB,
  API_TAB,
  type Connector,
} from "@/catalog";
import type { SourceInfo } from "@/gen/pawrly/v1/common_pb";
import { ConnectorIcon } from "@/components/ConnectorIcon";
import { PageHeader } from "@/components/PageHeader";
import { EmptyHint, TableSurface } from "@/components/PanelShell";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { DropdownMenu, DropdownItem } from "@/components/ui/dropdown-menu";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { SourceGalleryDialog } from "./SourceGalleryDialog";
import { SourceConnectDialog } from "./SourceConnectDialog";
import { SourceDetailPage } from "./SourceDetailPage";

/** Read/write the `?connector=<name>` detail param, mirroring App's `?page=`. */
function useConnectorParam(): [string | null, (name: string | null) => void] {
  const read = () =>
    new URLSearchParams(window.location.search).get("connector");
  const [name, setName] = useState<string | null>(read);
  useEffect(() => {
    const onPop = () => setName(read());
    window.addEventListener("popstate", onPop);
    return () => window.removeEventListener("popstate", onPop);
  }, []);
  const navigate = useCallback((next: string | null) => {
    const url = new URL(window.location.href);
    if (next) url.searchParams.set("connector", next);
    else url.searchParams.delete("connector");
    window.history.pushState({ ...window.history.state }, "", url);
    setName(next);
  }, []);
  return [name, navigate];
}

export function SourcesPanel() {
  const { sources, query } = useClients();
  const { openSql } = useNav();
  const [openName, setOpenName] = useConnectorParam();

  const list = useAsync(
    () => sources.listSources({}).then((r) => r.sources),
    [sources],
  );
  const items = useMemo(() => list.data ?? [], [list.data]);

  // Per-source usage (queries / last-queried) from system.activity; null when
  // the activity table isn't enabled.
  const usage = useAsync(
    () => loadSourceUsage(query, items.map((s) => s.name)),
    [query, items],
  );

  const openSource = openName
    ? items.find((s) => s.name === openName)
    : undefined;

  if (openSource) {
    return (
      <SourceDetailPage
        source={openSource}
        onBack={() => setOpenName(null)}
        onRemoved={() => {
          setOpenName(null);
          list.reload();
        }}
        onOpenSql={openSql}
      />
    );
  }

  return (
    <ConnectorsList
      items={items}
      loading={list.loading}
      error={list.error}
      usage={usage.data ?? null}
      onReload={() => {
        list.reload();
        usage.reload();
      }}
      onOpen={(name) => setOpenName(name)}
    />
  );
}

type SortKey = "name" | "type" | "queries" | "created";
type SortDir = "asc" | "desc";

function ConnectorsList({
  items,
  loading,
  error,
  usage,
  onReload,
  onOpen,
}: {
  items: SourceInfo[];
  loading: boolean;
  error: string | null;
  usage: Record<string, SourceUsage> | null;
  onReload: () => void;
  onOpen: (name: string) => void;
}) {
  const { sources } = useClients();
  const [q, setQ] = useState("");
  const [sort, setSort] = useState<{ key: SortKey; dir: SortDir }>({
    key: "name",
    dir: "asc",
  });
  const [page, setPage] = useState(0);
  const [perPage, setPerPage] = useState(10);
  const [gallery, setGallery] = useState(false);
  const [picked, setPicked] = useState<Connector | null>(null);
  const [rowError, setRowError] = useState<string | null>(null);

  const typeLabel = useCallback((s: SourceInfo) => {
    return (
      connectorForSource(s.name, sourceKindLabel(s.kind))?.label ??
      sourceKindLabel(s.kind)
    );
  }, []);

  const connectedCounts = useMemo(() => {
    const counts: Record<string, number> = {};
    for (const s of items) {
      const c = connectorForSource(s.name, sourceKindLabel(s.kind));
      if (c) counts[c.id] = (counts[c.id] ?? 0) + 1;
    }
    return counts;
  }, [items]);

  const filtered = useMemo(() => {
    const needle = q.trim().toLowerCase();
    const base = needle
      ? items.filter(
          (s) =>
            s.name.toLowerCase().includes(needle) ||
            typeLabel(s).toLowerCase().includes(needle),
        )
      : items.slice();
    const dir = sort.dir === "asc" ? 1 : -1;
    base.sort((a, b) => {
      let cmp = 0;
      switch (sort.key) {
        case "name":
          cmp = a.name.localeCompare(b.name);
          break;
        case "type":
          cmp = typeLabel(a).localeCompare(typeLabel(b));
          break;
        case "queries":
          cmp = (usage?.[a.name]?.queries ?? 0) - (usage?.[b.name]?.queries ?? 0);
          break;
        case "created": {
          const at = (s: SourceInfo) => Number(s.registeredAt?.seconds ?? 0n);
          cmp = at(a) - at(b);
          break;
        }
      }
      return cmp * dir;
    });
    return base;
  }, [items, q, sort, usage, typeLabel]);

  const pageCount = Math.max(1, Math.ceil(filtered.length / perPage));
  const clampedPage = Math.min(page, pageCount - 1);
  const start = clampedPage * perPage;
  const pageRows = filtered.slice(start, start + perPage);

  function toggleSort(key: SortKey) {
    setSort((s) =>
      s.key === key
        ? { key, dir: s.dir === "asc" ? "desc" : "asc" }
        : { key, dir: "asc" },
    );
  }

  async function handleRemove(name: string) {
    setRowError(null);
    try {
      await sources.removeSource({ name });
      onReload();
    } catch (e) {
      setRowError(errMsg(e));
    }
  }

  function pickConnector(c: Connector) {
    setGallery(false);
    setPicked(c);
  }

  return (
    <div className="space-y-5">
      <PageHeader
        title="Connectors"
        description="Connect your data sources to start querying."
        actions={
          <Button onClick={() => setGallery(true)}>
            <Plus className="size-4" /> New Connector
          </Button>
        }
      />

      <div className="flex items-center gap-3">
        <div className="relative flex-1">
          <Search className="text-muted-foreground absolute top-1/2 left-3 size-4 -translate-y-1/2" />
          <Input
            value={q}
            onChange={(e) => {
              setQ(e.target.value);
              setPage(0);
            }}
            placeholder="Search active connectors…"
            className="pl-9"
          />
        </div>
        <Button variant="outline" size="sm" onClick={onReload} disabled={loading}>
          Reload
        </Button>
      </div>

      {error ? (
        <div className="text-destructive border-destructive/30 bg-destructive/5 rounded-lg border p-3 font-mono text-sm break-all">
          {error}
        </div>
      ) : items.length === 0 && !loading ? (
        <EmptyHint>
          No connectors yet. Click <span className="font-medium">New Connector</span>{" "}
          to add one.
        </EmptyHint>
      ) : (
        <TableSurface>
          <Table>
            <TableHeader>
              <TableRow>
                <SortHead label="Name" col="name" sort={sort} onSort={toggleSort} />
                <SortHead label="Type" col="type" sort={sort} onSort={toggleSort} />
                <SortHead
                  label="Queries"
                  col="queries"
                  sort={sort}
                  onSort={toggleSort}
                  className="text-right"
                />
                <TableHead>Last Queried</TableHead>
                <SortHead label="Created" col="created" sort={sort} onSort={toggleSort} />
                <TableHead className="w-8" />
              </TableRow>
            </TableHeader>
            <TableBody>
              {pageRows.map((s) => {
                const u = usage?.[s.name];
                return (
                  <TableRow
                    key={s.name}
                    onClick={() => onOpen(s.name)}
                    className="cursor-pointer"
                  >
                    <TableCell>
                      <div className="flex items-center gap-2.5">
                        <ConnectorIcon
                          seed={
                            connectorForSource(s.name, sourceKindLabel(s.kind))?.id ??
                            s.name
                          }
                          label={typeLabel(s)}
                          className="size-7 text-xs"
                        />
                        <span className="font-medium">{s.name}</span>
                      </div>
                    </TableCell>
                    <TableCell className="text-muted-foreground">{typeLabel(s)}</TableCell>
                    <TableCell className="text-right tabular-nums">
                      {u ? u.queries : "—"}
                    </TableCell>
                    <TableCell className="text-muted-foreground tabular-nums">
                      {u?.lastQueried
                        ? new Date(u.lastQueried).toLocaleString()
                        : "—"}
                    </TableCell>
                    <TableCell className="text-muted-foreground">
                      {formatTimestamp(s.registeredAt)}
                    </TableCell>
                    <TableCell>
                      <DropdownMenu
                        trigger={
                          <Button variant="ghost" size="icon" className="size-8">
                            <MoreHorizontal className="size-4" />
                          </Button>
                        }
                      >
                        <DropdownItem onSelect={() => onOpen(s.name)}>Open</DropdownItem>
                        <DropdownItem destructive onSelect={() => handleRemove(s.name)}>
                          Remove
                        </DropdownItem>
                      </DropdownMenu>
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>

          <Pagination
            total={filtered.length}
            page={clampedPage}
            perPage={perPage}
            pageCount={pageCount}
            onPage={setPage}
            onPerPage={(n) => {
              setPerPage(n);
              setPage(0);
            }}
          />
        </TableSurface>
      )}

      {rowError ? (
        <p className="text-destructive font-mono text-xs break-all">{rowError}</p>
      ) : null}

      <AddMore onPick={pickConnector} />

      <SourceGalleryDialog
        open={gallery}
        onClose={() => setGallery(false)}
        onPick={pickConnector}
        connectedCounts={connectedCounts}
      />
      <SourceConnectDialog
        connector={picked}
        onBack={() => {
          setPicked(null);
          setGallery(true);
        }}
        onClose={() => setPicked(null)}
        onSaved={(name) => {
          setPicked(null);
          onReload();
          onOpen(name);
        }}
      />
    </div>
  );
}

function SortHead({
  label,
  col,
  sort,
  onSort,
  className,
}: {
  label: string;
  col: SortKey;
  sort: { key: SortKey; dir: SortDir };
  onSort: (key: SortKey) => void;
  className?: string;
}) {
  const active = sort.key === col;
  return (
    <TableHead className={className}>
      <button
        onClick={() => onSort(col)}
        className={cn(
          "hover:text-foreground inline-flex items-center gap-1 uppercase",
          className?.includes("text-right") && "flex-row-reverse",
        )}
      >
        {label}
        {active ? (
          sort.dir === "asc" ? (
            <ChevronUp className="size-3" />
          ) : (
            <ChevronDown className="size-3" />
          )
        ) : null}
      </button>
    </TableHead>
  );
}

function Pagination({
  total,
  page,
  perPage,
  pageCount,
  onPage,
  onPerPage,
}: {
  total: number;
  page: number;
  perPage: number;
  pageCount: number;
  onPage: (p: number) => void;
  onPerPage: (n: number) => void;
}) {
  const from = total === 0 ? 0 : page * perPage + 1;
  const to = Math.min(total, (page + 1) * perPage);
  return (
    <div className="text-muted-foreground flex flex-wrap items-center justify-between gap-3 border-t px-3 py-2.5 text-sm">
      <span>
        Showing {from} to {to} of {total} results
      </span>
      <div className="flex items-center gap-1">
        <PagerButton onClick={() => onPage(0)} disabled={page === 0}>
          <ChevronsLeft className="size-4" />
        </PagerButton>
        <PagerButton onClick={() => onPage(page - 1)} disabled={page === 0}>
          <ChevronLeft className="size-4" />
        </PagerButton>
        <span className="bg-primary text-primary-foreground flex h-7 min-w-7 items-center justify-center rounded-md px-2 text-xs font-medium">
          {page + 1}
        </span>
        <PagerButton onClick={() => onPage(page + 1)} disabled={page >= pageCount - 1}>
          <ChevronRight className="size-4" />
        </PagerButton>
        <PagerButton onClick={() => onPage(pageCount - 1)} disabled={page >= pageCount - 1}>
          <ChevronsRight className="size-4" />
        </PagerButton>
      </div>
      <label className="flex items-center gap-2">
        Show:
        <select
          value={perPage}
          onChange={(e) => onPerPage(Number(e.target.value))}
          className="border-input h-7 rounded-md border bg-transparent px-2 text-sm"
        >
          {[5, 10, 25, 50].map((n) => (
            <option key={n} value={n}>
              {n}
            </option>
          ))}
        </select>
        per page
      </label>
    </div>
  );
}

function PagerButton({
  onClick,
  disabled,
  children,
}: {
  onClick: () => void;
  disabled?: boolean;
  children: ReactNode;
}) {
  return (
    <button
      onClick={onClick}
      disabled={disabled}
      className="hover:bg-accent flex size-7 items-center justify-center rounded-md border disabled:pointer-events-none disabled:opacity-40"
    >
      {children}
    </button>
  );
}

/** "Add More Data Sources" tiles: all databases + a few popular APIs. */
function AddMore({ onPick }: { onPick: (c: Connector) => void }) {
  const popularApiIds = [
    "linear",
    "github",
    "notion",
    "slack",
    "jira",
    "stripe",
    "sentry",
    "datadog",
  ];
  const tiles = [
    ...DATABASE_TAB,
    ...popularApiIds
      .map((id) => API_TAB.find((c) => c.id === id))
      .filter((c): c is Connector => !!c),
  ];

  return (
    <div className="bg-card rounded-lg border p-5">
      <h2 className="text-sm font-semibold">Add More Data Sources</h2>
      <p className="text-muted-foreground mt-0.5 text-sm">
        Connect additional data sources to unlock more insights.
      </p>
      <div className="mt-4 grid grid-cols-3 gap-2 sm:grid-cols-4 md:grid-cols-6 lg:grid-cols-8">
        {tiles.map((c) => (
          <button
            key={c.id}
            onClick={() => onPick(c)}
            className="hover:border-primary/40 hover:bg-accent/40 flex flex-col items-center gap-2 rounded-lg border p-3 text-center transition-colors"
            title={c.subtitle}
          >
            <ConnectorIcon seed={c.id} label={c.label} />
            <span className="w-full truncate text-xs font-medium">{c.label}</span>
          </button>
        ))}
      </div>
    </div>
  );
}
