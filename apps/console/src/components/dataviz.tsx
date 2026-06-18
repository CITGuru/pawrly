import { cn } from "@/lib/utils";

export function MiniBar({
  value,
  max,
  tone = "primary",
  className,
}: {
  value: number;
  max: number;
  tone?: "primary" | "success" | "muted";
  className?: string;
}) {
  const pct = max > 0 ? Math.min(100, (value / max) * 100) : 0;
  const fill =
    tone === "success"
      ? "bg-success/70"
      : tone === "muted"
        ? "bg-muted-foreground/40"
        : "bg-primary/70";
  return (
    <div className={cn("bg-muted h-1.5 w-24 overflow-hidden rounded-full", className)}>
      <div
        className={cn("h-full rounded-full", fill)}
        style={{ width: `${pct}%` }}
      />
    </div>
  );
}

export function StatusDot({
  ok,
  label,
  title,
}: {
  ok: boolean;
  label: string;
  title?: string;
}) {
  return (
    <span className="inline-flex items-center gap-2" title={title}>
      <span
        className={cn(
          "size-2 shrink-0 rounded-full",
          ok ? "bg-success" : "bg-destructive",
        )}
      />
      <span className={ok ? "" : "text-destructive"}>{label}</span>
    </span>
  );
}
