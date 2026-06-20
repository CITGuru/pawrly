/**
 * The palm-paw — an alternative mark. Read it top-down and it's a palm tree:
 * a fan of fronds on a slim trunk rising from a rounded base. Read it as a
 * silhouette and the base is the paw pad (carrying the same `>_` SQL cursor)
 * and the fronds sit where the four toe-beans would. Same API as PawMark so
 * the two are drop-in interchangeable.
 */
const FRONDS = [-44, -22, 0, 22, 44];

export function PalmMark({
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
      {/* canopy — fronds fanning from the trunk top */}
      {FRONDS.map((deg) => (
        <path
          key={deg}
          d="M60 52 C54 42 55 26 60 16 C65 26 66 42 60 52 Z"
          fill={pads}
          transform={`rotate(${deg} 60 52)`}
        />
      ))}
      {/* trunk */}
      <path d="M57.5 68 C58.3 63 58.6 58 59 52 L61 52 C61.4 58 61.7 63 62.5 68 Z" fill={pads} />
      {/* base = paw pad */}
      <path
        d="M60 66 C77 66 91 77 91 90 C91 103 78 112 60 112 C42 112 29 103 29 90 C29 77 43 66 60 66 Z"
        fill={pads}
      />
      {/* carved >_ SQL cursor */}
      <g
        fill="none"
        stroke={carve}
        strokeWidth="4.5"
        strokeLinecap="round"
        strokeLinejoin="round"
      >
        <path d="M52 84 L60 92 L52 100" />
        <path d="M64 100 L72 100" />
      </g>
    </svg>
  );
}
