import * as React from "react";
import { ChevronDown } from "lucide-react";

import { cn } from "@/lib/utils";

/** A lightweight disclosure (the "▾ Advanced" section in the connect form). */
export function Collapsible({
  label,
  defaultOpen = false,
  children,
  className,
}: {
  label: string;
  defaultOpen?: boolean;
  children: React.ReactNode;
  className?: string;
}) {
  const [open, setOpen] = React.useState(defaultOpen);
  return (
    <div className={className}>
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        className="text-muted-foreground hover:text-foreground flex items-center gap-1 text-sm font-medium"
      >
        <ChevronDown
          className={cn("size-4 transition-transform", open ? "" : "-rotate-90")}
        />
        {label}
      </button>
      {open ? <div className="mt-3 space-y-3">{children}</div> : null}
    </div>
  );
}
