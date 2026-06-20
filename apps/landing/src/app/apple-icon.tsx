import { ImageResponse } from "next/og";

// The apple-icon file convention only registers a route for raster formats
// (png/jpg), not svg — so we render the touch icon as a PNG via ImageResponse:
// a cream paw with the carved >_ cursor on a solid ocean tile (iOS masks it).
export const size = { width: 180, height: 180 };
export const contentType = "image/png";

const MARK = `<svg xmlns="http://www.w3.org/2000/svg" width="120" height="120" viewBox="0 0 120 120">
<ellipse cx="30" cy="58" rx="9" ry="12" fill="#f4efe4" transform="rotate(-20 30 58)"/>
<ellipse cx="48" cy="40" rx="9.5" ry="13" fill="#f4efe4" transform="rotate(-8 48 40)"/>
<ellipse cx="72" cy="40" rx="9.5" ry="13" fill="#f4efe4" transform="rotate(8 72 40)"/>
<ellipse cx="90" cy="58" rx="9" ry="12" fill="#f4efe4" transform="rotate(20 90 58)"/>
<path d="M60 60 C78 60 92 72 92 88 C92 104 78 112 60 112 C42 112 28 104 28 88 C28 72 42 60 60 60 Z" fill="#f4efe4"/>
<g fill="none" stroke="#0a2233" stroke-width="4.5" stroke-linecap="round" stroke-linejoin="round">
<path d="M52 80 L60 88 L52 96"/><path d="M64 96 L72 96"/></g></svg>`;

const markSrc = `data:image/svg+xml,${encodeURIComponent(MARK)}`;

export default function AppleIcon() {
  return new ImageResponse(
    (
      <div
        style={{
          width: "100%",
          height: "100%",
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          backgroundImage: "linear-gradient(180deg, #16456a 0%, #081d2f 100%)",
        }}
      >
        {/* eslint-disable-next-line @next/next/no-img-element */}
        <img src={markSrc} width={128} height={128} alt="" />
      </div>
    ),
    { ...size }
  );
}
