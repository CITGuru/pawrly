import { ImageResponse } from "next/og";

export const OG_SIZE = { width: 1200, height: 630 };
export const OG_CONTENT_TYPE = "image/png";

// The paw mark, cream on a transparent ground — embedded as a data-URI image so
// Satori rasterizes the rotated toe-beans faithfully.
const MARK = `<svg xmlns="http://www.w3.org/2000/svg" width="120" height="120" viewBox="0 0 120 120">
<ellipse cx="30" cy="58" rx="9" ry="12" fill="#f4efe4" transform="rotate(-20 30 58)"/>
<ellipse cx="48" cy="40" rx="9.5" ry="13" fill="#f4efe4" transform="rotate(-8 48 40)"/>
<ellipse cx="72" cy="40" rx="9.5" ry="13" fill="#f4efe4" transform="rotate(8 72 40)"/>
<ellipse cx="90" cy="58" rx="9" ry="12" fill="#f4efe4" transform="rotate(20 90 58)"/>
<path d="M60 60 C78 60 92 72 92 88 C92 104 78 112 60 112 C42 112 28 104 28 88 C28 72 42 60 60 60 Z" fill="#f4efe4"/>
<g fill="none" stroke="#0a2233" stroke-width="4.5" stroke-linecap="round" stroke-linejoin="round">
<path d="M52 80 L60 88 L52 96"/><path d="M64 96 L72 96"/></g></svg>`;
const markSrc = `data:image/svg+xml,${encodeURIComponent(MARK)}`;

/** Shared Pawrly OG card: ocean gradient, paw mark, optional eyebrow + title. */
export function ogImage({
  eyebrow,
  title,
  accent,
  tagline = "Connect once · Query from anywhere",
  fontSize = 64,
}: {
  eyebrow?: string;
  title: string;
  accent?: string;
  tagline?: string;
  fontSize?: number;
}) {
  // Render the title word-by-word so it wraps like normal text. (Satori lays a
  // flex container with mixed text + spans as overlapping items, not inline.)
  const aStart = accent ? title.indexOf(accent) : -1;
  const aEnd = aStart >= 0 ? aStart + (accent as string).length : -1;
  const words: { text: string; on: boolean }[] = [];
  for (const m of title.matchAll(/\S+/g)) {
    const s = m.index ?? 0;
    const e = s + m[0].length;
    words.push({ text: m[0], on: aStart >= 0 && s >= aStart && e <= aEnd });
  }

  return new ImageResponse(
    (
      <div
        style={{
          width: "100%",
          height: "100%",
          display: "flex",
          flexDirection: "column",
          padding: "72px 80px",
          backgroundColor: "#05121f",
          backgroundImage:
            "radial-gradient(900px 520px at 78% -8%, rgba(231,195,137,0.22), rgba(231,195,137,0) 60%), linear-gradient(155deg, #0e3149 0%, #0a2233 48%, #05121f 100%)",
          fontFamily: "Georgia, 'Times New Roman', serif",
        }}
      >
        <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between" }}>
          <div style={{ display: "flex", alignItems: "center", gap: 18 }}>
            {/* eslint-disable-next-line @next/next/no-img-element */}
            <img src={markSrc} width={54} height={54} alt="" />
            <div
              style={{
                fontSize: 38,
                fontWeight: 600,
                color: "#f4efe4",
                letterSpacing: "-0.02em",
                fontFamily: "system-ui, sans-serif",
              }}
            >
              pawrly
            </div>
          </div>
          {eyebrow ? (
            <div
              style={{
                fontSize: 20,
                letterSpacing: "0.2em",
                textTransform: "uppercase",
                color: "#e7c389",
                fontFamily: "system-ui, sans-serif",
              }}
            >
              {eyebrow}
            </div>
          ) : null}
        </div>

        <div style={{ display: "flex", flexGrow: 1, alignItems: "center" }}>
          <div
            style={{
              display: "flex",
              flexWrap: "wrap",
              fontSize,
              letterSpacing: "-0.02em",
              lineHeight: 1.12,
              maxWidth: 1010,
            }}
          >
            {words.map((w, i) => (
              <span key={i} style={{ color: w.on ? "#f1dcb0" : "#f4efe4", marginRight: "0.28em" }}>
                {w.text}
              </span>
            ))}
          </div>
        </div>

        <div
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            fontSize: 24,
            color: "#bcd2e6",
            fontFamily: "system-ui, sans-serif",
          }}
        >
          <span>pawrly.dev</span>
          <span style={{ color: "#7f99ad" }}>{tagline}</span>
        </div>
      </div>
    ),
    { ...OG_SIZE }
  );
}
