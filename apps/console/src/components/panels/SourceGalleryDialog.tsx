import { useMemo, useState } from "react";
import { Plus, Search } from "lucide-react";

import { cn } from "@/lib/utils";
import {
  connectorsForCategory,
  type Connector,
  type ConnectorCategory,
} from "@/catalog";
import { ConnectorIcon } from "@/components/ConnectorIcon";
import { Input } from "@/components/ui/input";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";

export function SourceGalleryDialog({
  open,
  onClose,
  onPick,
  connectedCounts,
}: {
  open: boolean;
  onClose: () => void;
  onPick: (connector: Connector) => void;
  /** connector id → number of live sources of that connector. */
  connectedCounts: Record<string, number>;
}) {
  const [tab, setTab] = useState<ConnectorCategory>("database");
  const [q, setQ] = useState("");

  return (
    <Dialog
      open={open}
      onOpenChange={(o) => {
        if (!o) onClose();
      }}
    >
      <DialogContent className="max-w-3xl">
        <DialogHeader>
          <DialogTitle>Create a New Connector</DialogTitle>
        </DialogHeader>

        <Tabs
          value={tab}
          onValueChange={(v) => setTab(v as ConnectorCategory)}
          className="gap-4"
        >
          <div className="flex items-center justify-between gap-3">
            <TabsList>
              <TabsTrigger value="database">Databases</TabsTrigger>
              <TabsTrigger value="api">APIs</TabsTrigger>
            </TabsList>
          </div>

          <div className="relative">
            <Search className="text-muted-foreground absolute top-1/2 left-3 size-4 -translate-y-1/2" />
            <Input
              value={q}
              onChange={(e) => setQ(e.target.value)}
              placeholder="Search"
              className="pl-9"
              autoFocus
            />
          </div>

          <TabsContent value="database">
            <Grid
              connectors={connectorsForCategory("database")}
              query={q}
              connectedCounts={connectedCounts}
              onPick={onPick}
            />
          </TabsContent>
          <TabsContent value="api">
            <Grid
              connectors={connectorsForCategory("api")}
              query={q}
              connectedCounts={connectedCounts}
              onPick={onPick}
            />
          </TabsContent>
        </Tabs>
      </DialogContent>
    </Dialog>
  );
}

function Grid({
  connectors,
  query,
  connectedCounts,
  onPick,
}: {
  connectors: Connector[];
  query: string;
  connectedCounts: Record<string, number>;
  onPick: (c: Connector) => void;
}) {
  const filtered = useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (!needle) return connectors;
    return connectors.filter(
      (c) =>
        c.label.toLowerCase().includes(needle) ||
        c.subtitle.toLowerCase().includes(needle),
    );
  }, [connectors, query]);

  if (filtered.length === 0) {
    return (
      <p className="text-muted-foreground py-10 text-center text-sm">
        No connectors match “{query}”.
      </p>
    );
  }

  return (
    <div className="grid max-h-[55vh] grid-cols-1 gap-3 overflow-y-auto pr-1 sm:grid-cols-2">
      {filtered.map((c) => {
        const count = connectedCounts[c.id] ?? 0;
        return (
          <button
            key={c.id}
            type="button"
            onClick={() => onPick(c)}
            className={cn(
              "group hover:border-primary/40 hover:bg-accent/40 flex items-center gap-3 rounded-lg border p-3 text-left transition-colors",
              c.custom && "border-dashed",
            )}
          >
            <ConnectorIcon seed={c.id} label={c.label} />
            <div className="min-w-0 flex-1 leading-tight">
              <div className="truncate text-sm font-semibold">{c.label}</div>
              {count > 0 ? (
                <div className="text-success flex items-center gap-1.5 text-xs">
                  <span className="bg-success size-1.5 rounded-full" />
                  {count} connected
                </div>
              ) : (
                <div className="text-muted-foreground truncate text-xs">
                  {c.subtitle}
                </div>
              )}
            </div>
            <Plus className="text-muted-foreground group-hover:text-foreground size-4 shrink-0" />
          </button>
        );
      })}
    </div>
  );
}
