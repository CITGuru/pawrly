import type { ReactNode } from "react";
import { AlertTriangle, RefreshCw } from "lucide-react";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { PageHeader } from "@/components/PageHeader";

interface PanelShellProps {
  title: string;
  description?: string;
  loading?: boolean;
  error?: string | null;
  onReload?: () => void;
  stats?: ReactNode;
  children: ReactNode;
}

export function PanelShell({
  title,
  description,
  loading,
  error,
  onReload,
  stats,
  children,
}: PanelShellProps) {
  return (
    <div className="space-y-5">
      <PageHeader
        title={title}
        description={description}
        actions={
          onReload ? (
            <Button
              variant="outline"
              size="sm"
              onClick={onReload}
              disabled={loading}
            >
              <RefreshCw className={cn("size-4", loading && "animate-spin")} />
              Reload
            </Button>
          ) : null
        }
      />
      {stats}
      {error ? (
        <div className="text-destructive border-destructive/30 bg-destructive/5 flex items-start gap-2 rounded-lg border p-3 text-sm">
          <AlertTriangle className="mt-0.5 size-4 shrink-0" />
          <span className="font-mono break-all">{error}</span>
        </div>
      ) : (
        children
      )}
    </div>
  );
}

export function TableSurface({ children }: { children: ReactNode }) {
  return (
    <div className="bg-card overflow-hidden rounded-lg border">{children}</div>
  );
}

export function EmptyHint({ children }: { children: ReactNode }) {
  return (
    <div className="text-muted-foreground bg-card rounded-lg border px-4 py-12 text-center text-sm">
      {children}
    </div>
  );
}
