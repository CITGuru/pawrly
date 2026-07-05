import { cn } from "@/lib/utils";

/** Stable hue in [0,360) from a seed string. */
function hashHue(seed: string): number {
  let h = 0;
  for (let i = 0; i < seed.length; i++) {
    h = (h * 31 + seed.charCodeAt(i)) >>> 0;
  }
  return h % 360;
}

/**
 * A rounded monogram tile for a connector — a colored initial keyed off the
 * connector id, so each source is visually distinct without bundling brand
 * logos. (Swapping in real SVG logos is a later polish.)
 */
export function ConnectorIcon({
  seed,
  label,
  className,
}: {
  seed: string;
  label: string;
  className?: string;
}) {
  const hue = hashHue(seed || label);
  const initial = (label.trim()[0] ?? "?").toUpperCase();
  return (
    <span
      aria-hidden
      className={cn(
        "flex size-9 shrink-0 items-center justify-center rounded-lg border text-sm font-semibold select-none",
        className,
      )}
      style={{
        backgroundColor: `hsl(${hue} 60% 50% / 0.12)`,
        color: `hsl(${hue} 50% 38%)`,
        borderColor: `hsl(${hue} 50% 50% / 0.2)`,
      }}
    >
      {initial}
    </span>
  );
}
