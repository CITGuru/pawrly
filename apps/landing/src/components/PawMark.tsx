/**
 * Pawrly's mark — four toe-beans over a palm pad, with a SQL prompt (`>_`)
 * carved into the pad as negative space. Rendered monochrome here so it reads
 * the way the reference does: a single warm-cream shape on deep water.
 *
 * `pads` paints the beans + pad (defaults to currentColor, so the surrounding
 * text color drives it). `carve` is the cursor cut out of the pad — set it to
 * whatever sits behind the mark.
 */
export function PawMark({
  className = "",
  size = 28,
  pads = "currentColor",
  carve = "var(--ocean-950)",
}: {
  className?: string;
  size?: number;
  pads?: string;
  carve?: string;
}) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 120 120"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      className={className}
      role="img"
      aria-label="Pawrly"
    >
      <ellipse cx="30" cy="58" rx="9" ry="12" fill={pads} transform="rotate(-20 30 58)" />
      <ellipse cx="48" cy="40" rx="9.5" ry="13" fill={pads} transform="rotate(-8 48 40)" />
      <ellipse cx="72" cy="40" rx="9.5" ry="13" fill={pads} transform="rotate(8 72 40)" />
      <ellipse cx="90" cy="58" rx="9" ry="12" fill={pads} transform="rotate(20 90 58)" />
      <path
        d="M60 60 C78 60 92 72 92 88 C92 104 78 112 60 112 C42 112 28 104 28 88 C28 72 42 60 60 60 Z"
        fill={pads}
      />
      <g
        fill="none"
        stroke={carve}
        strokeWidth="4.5"
        strokeLinecap="round"
        strokeLinejoin="round"
      >
        <path d="M52 80 L60 88 L52 96" />
        <path d="M64 96 L72 96" />
      </g>
    </svg>
  );
}
