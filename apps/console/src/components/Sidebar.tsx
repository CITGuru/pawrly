import type { ComponentType } from "react";
import {
  Activity,
  Boxes,
  Database,
  Layers,
  PawPrint,
  Shapes,
  Table2,
  Terminal,
} from "lucide-react";

import { cn } from "@/lib/utils";
import { useClients, useConnection } from "@/lib/connection";
import { useAsync } from "@/lib/useAsync";
import { Input } from "@/components/ui/input";

export type PageId =
  | "sources"
  | "catalog"
  | "semantic"
  | "cache"
  | "materialized"
  | "activity"
  | "sql";

interface NavItem {
  id: PageId;
  label: string;
  icon: ComponentType<{ className?: string }>;
}

const NAV: { group: string; items: NavItem[] }[] = [
  {
    group: "Workspace",
    items: [
      { id: "sources", label: "Sources", icon: Boxes },
      { id: "catalog", label: "Catalog", icon: Table2 },
      { id: "semantic", label: "Semantic", icon: Shapes },
    ],
  },
  {
    group: "Storage",
    items: [
      { id: "cache", label: "Cache", icon: Database },
      { id: "materialized", label: "Materialized", icon: Layers },
    ],
  },
  {
    group: "Observe",
    items: [{ id: "activity", label: "Activity", icon: Activity }],
  },
  {
    group: "Tools",
    items: [{ id: "sql", label: "SQL runner", icon: Terminal }],
  },
];

export const PAGE_LABELS: Record<PageId, string> = Object.fromEntries(
  NAV.flatMap((s) => s.items.map((i) => [i.id, i.label])),
) as Record<PageId, string>;

function useHealth() {
  const { admin } = useClients();
  const state = useAsync(() => admin.health({}), [admin]);
  const tone = state.error
    ? "bg-destructive"
    : state.data
      ? state.data.ok
        ? "bg-success"
        : "bg-warning"
      : "bg-muted-foreground animate-pulse";
  const label = state.error
    ? "unreachable"
    : state.data
      ? state.data.ok
        ? `healthy · v${state.data.version}`
        : "degraded"
      : "connecting…";
  return { tone, label };
}

export function Sidebar({
  active,
  onNavigate,
  collapsed,
}: {
  active: PageId;
  onNavigate: (id: PageId) => void;
  collapsed: boolean;
}) {
  const { baseUrl, setBaseUrl, token, setToken, traceUrlTemplate, setTraceUrlTemplate } =
    useConnection();
  const health = useHealth();

  return (
    <aside
      className={cn(
        "bg-sidebar border-sidebar-border flex shrink-0 flex-col border-r transition-[width] duration-200",
        collapsed ? "w-14" : "w-60",
      )}
    >
      <div
        className={cn(
          "flex items-center gap-2 py-4",
          collapsed ? "justify-center px-2" : "px-4",
        )}
      >
        <div className="bg-primary text-primary-foreground flex size-7 shrink-0 items-center justify-center rounded-md">
          <PawPrint className="size-4" />
        </div>
        {!collapsed ? (
          <div className="leading-tight">
            <div className="text-sidebar-accent-foreground text-sm font-semibold">
              pawrly
            </div>
            <div className="text-sidebar-foreground text-[11px]">Console</div>
          </div>
        ) : null}
      </div>

      <nav
        className={cn(
          "flex-1 overflow-y-auto py-2",
          collapsed ? "space-y-1 px-2" : "space-y-5 px-3",
        )}
      >
        {NAV.map((section) => (
          <div key={section.group} className={collapsed ? "space-y-1" : ""}>
            {!collapsed ? (
              <div className="text-sidebar-foreground/70 px-2 pb-1 text-[10px] font-semibold tracking-widest uppercase">
                {section.group}
              </div>
            ) : null}
            <div className="space-y-0.5">
              {section.items.map((item) => {
                const isActive = item.id === active;
                const Icon = item.icon;
                return (
                  <button
                    key={item.id}
                    onClick={() => onNavigate(item.id)}
                    title={collapsed ? item.label : undefined}
                    className={cn(
                      "flex items-center rounded-md text-sm transition-colors",
                      collapsed
                        ? "size-9 justify-center"
                        : "w-full gap-2.5 px-2 py-1.5",
                      isActive
                        ? "bg-sidebar-accent text-sidebar-accent-foreground font-medium"
                        : "text-sidebar-foreground hover:bg-sidebar-accent/60 hover:text-sidebar-accent-foreground",
                    )}
                  >
                    <Icon className="size-4 shrink-0" />
                    {!collapsed ? item.label : null}
                  </button>
                );
              })}
            </div>
          </div>
        ))}
      </nav>

      <div
        className={cn(
          "border-sidebar-border border-t",
          collapsed ? "flex justify-center px-2 py-3" : "space-y-2 p-3",
        )}
      >
        {collapsed ? (
          <span
            className={cn("size-2.5 rounded-full", health.tone)}
            title={health.label}
          />
        ) : (
          <>
            <label className="block">
              <span className="text-sidebar-foreground/70 text-[10px] font-semibold tracking-widest uppercase">
                Endpoint
              </span>
              <Input
                value={baseUrl}
                onChange={(e) => setBaseUrl(e.target.value)}
                spellCheck={false}
                className="mt-1 h-8 font-mono text-xs"
              />
            </label>
            <label className="block">
              <span className="text-sidebar-foreground/70 text-[10px] font-semibold tracking-widest uppercase">
                Token
              </span>
              <Input
                value={token}
                onChange={(e) => setToken(e.target.value)}
                type="password"
                placeholder="optional"
                className="mt-1 h-8 font-mono text-xs"
              />
            </label>
            <label className="block">
              <span className="text-sidebar-foreground/70 text-[10px] font-semibold tracking-widest uppercase">
                Trace URL
              </span>
              <Input
                value={traceUrlTemplate}
                onChange={(e) => setTraceUrlTemplate(e.target.value)}
                spellCheck={false}
                placeholder="https://jaeger/trace/{traceId}"
                className="mt-1 h-8 font-mono text-xs"
              />
            </label>
            <div className="flex items-center gap-2 text-xs">
              <span className={cn("size-2 shrink-0 rounded-full", health.tone)} />
              <span className="text-sidebar-foreground truncate">
                {health.label}
              </span>
            </div>
          </>
        )}
      </div>
    </aside>
  );
}
