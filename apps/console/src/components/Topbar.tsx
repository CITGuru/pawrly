import { PanelLeftClose, PanelLeftOpen } from "lucide-react";

import { Button } from "@/components/ui/button";
import { PAGE_LABELS, type PageId } from "@/components/Sidebar";

export function Topbar({
  page,
  collapsed,
  onToggle,
}: {
  page: PageId;
  collapsed: boolean;
  onToggle: () => void;
}) {
  return (
    <header className="bg-background/80 sticky top-0 z-10 flex h-12 shrink-0 items-center gap-3 border-b px-3 backdrop-blur">
      <Button
        variant="ghost"
        size="icon"
        onClick={onToggle}
        aria-label={collapsed ? "Expand sidebar" : "Collapse sidebar"}
        title={collapsed ? "Expand sidebar" : "Collapse sidebar"}
        className="size-8"
      >
        {collapsed ? (
          <PanelLeftOpen className="size-4" />
        ) : (
          <PanelLeftClose className="size-4" />
        )}
      </Button>
      <div className="text-sm">
        <span className="text-muted-foreground">pawrly</span>
        <span className="text-muted-foreground/50 mx-1.5">/</span>
        <span className="font-medium">{PAGE_LABELS[page]}</span>
      </div>
    </header>
  );
}
