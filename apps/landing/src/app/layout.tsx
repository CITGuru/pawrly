import type { Metadata } from "next";
import { Newsreader, Geist, Geist_Mono } from "next/font/google";
import "./globals.css";
import { DesignSwitcher } from "@/components/DesignSwitcher";

// Applies the saved/URL design choice before first paint so there's no flash of
// the default theme. Temporary — paired with <DesignSwitcher/>.
const NO_FLASH = `(function(){try{var p=new URLSearchParams(location.search),e=document.documentElement;var t=p.get('theme')||localStorage.getItem('pawrly:theme');var m=p.get('mark')||localStorage.getItem('pawrly:mark');if(t==='warm')e.setAttribute('data-theme','warm');if(m==='palm')e.setAttribute('data-mark','palm');}catch(e){}})();`;

// Serif display voice — calm, literary, a little hand-set. Carries the headlines.
const display = Newsreader({
  variable: "--font-display",
  subsets: ["latin"],
  weight: ["300", "400", "500", "600"],
  style: ["normal", "italic"],
});

// Clean sans for UI + body, mono for the SQL.
const sans = Geist({ variable: "--font-sans", subsets: ["latin"] });
const mono = Geist_Mono({ variable: "--font-mono", subsets: ["latin"] });

const DOCUMENT_TITLE = "Pawrly — Query APIs, files, MCP tools, and databases with SQL";
const SOCIAL_TITLE = "Query APIs, files, MCP tools, and databases with SQL";
const SHARED_DESCRIPTION =
  "Pawrly lets teams connect APIs, files, MCP tools, and databases, query them with SQL, and give agents the same reviewed workspace.";

// Setting an openGraph block suppresses Next's auto-attached opengraph-image, so
// thread the generated card through explicitly here and on pages that override OG.
const OG_IMAGE = {
  url: "/opengraph-image",
  width: 1200,
  height: 630,
  alt: SOCIAL_TITLE,
};

export const metadata: Metadata = {
  metadataBase: new URL("https://pawrly.dev"),
  title: {
    default: DOCUMENT_TITLE,
    template: "%s — Pawrly",
  },
  description: SHARED_DESCRIPTION,
  applicationName: "Pawrly",
  keywords: [
    "SQL over APIs",
    "query REST API with SQL",
    "OpenAPI to SQL",
    "MCP tools",
    "MCP server",
    "agent data access",
    "SQL for agents",
    "query files with SQL",
    "query databases with SQL",
    "Snowflake",
    "Iceberg",
    "no ETL",
  ],
  authors: [{ name: "Pawrly" }],
  alternates: { canonical: "/" },
  robots: { index: true, follow: true },
  openGraph: {
    type: "website",
    siteName: "Pawrly",
    title: SOCIAL_TITLE,
    description: SHARED_DESCRIPTION,
    url: "/",
    locale: "en_US",
    images: [OG_IMAGE],
  },
  twitter: {
    card: "summary_large_image",
    title: SOCIAL_TITLE,
    description: SHARED_DESCRIPTION,
    images: [OG_IMAGE.url],
  },
};

export default function RootLayout({
  children,
}: Readonly<{ children: React.ReactNode }>) {
  return (
    <html
      lang="en"
      className={`${display.variable} ${sans.variable} ${mono.variable} h-full antialiased`}
    >
      <body className="min-h-full flex flex-col bg-ocean text-ink">
        <script dangerouslySetInnerHTML={{ __html: NO_FLASH }} />
        {children}
        <DesignSwitcher />
      </body>
    </html>
  );
}
