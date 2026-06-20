import Link from "next/link";
import { ReactNode } from "react";

export function Eyebrow({ children, className = "" }: { children: ReactNode; className?: string }) {
  return (
    <span
      className={`inline-flex items-center gap-2 text-[11px] font-medium uppercase tracking-[0.22em] text-gold/90 ${className}`}
    >
      {children}
    </span>
  );
}

export function Pill({ children, className = "" }: { children: ReactNode; className?: string }) {
  return (
    <span
      className={`inline-flex items-center gap-1.5 rounded-full border border-line bg-card px-3 py-1 text-xs font-medium text-foam ${className}`}
    >
      {children}
    </span>
  );
}

export function PrimaryCTA({
  href,
  children,
  className = "",
  external,
}: {
  href: string;
  children: ReactNode;
  className?: string;
  external?: boolean;
}) {
  const Tag = external ? "a" : Link;
  const props = external ? { target: "_blank", rel: "noreferrer" } : {};
  return (
    <Tag
      href={href}
      {...props}
      className={`inline-flex items-center gap-2 rounded-full bg-sand px-5 py-3 text-sm font-semibold text-ocean-950 transition-colors hover:bg-gold-2 ${className}`}
    >
      {children}
      <ArrowRight />
    </Tag>
  );
}

export function GhostCTA({
  href,
  children,
  className = "",
  external,
}: {
  href: string;
  children: ReactNode;
  className?: string;
  external?: boolean;
}) {
  const Tag = external ? "a" : Link;
  const props = external ? { target: "_blank", rel: "noreferrer" } : {};
  return (
    <Tag
      href={href}
      {...props}
      className={`inline-flex items-center gap-2 rounded-full border border-line-2 bg-card px-5 py-3 text-sm font-medium text-cream transition-colors hover:bg-card-2 ${className}`}
    >
      {children}
    </Tag>
  );
}

export function ArrowRight({ size = 14 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" aria-hidden>
      <path
        d="M5 12h14M13 5l7 7-7 7"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

export function SectionHeader({
  eyebrow,
  title,
  description,
  align = "center",
}: {
  eyebrow?: string;
  title: ReactNode;
  description?: ReactNode;
  align?: "center" | "left";
}) {
  const alignCls =
    align === "center" ? "text-center mx-auto items-center" : "text-left items-start";
  return (
    <div className={`flex max-w-2xl flex-col gap-4 ${alignCls}`}>
      {eyebrow ? <Eyebrow>{eyebrow}</Eyebrow> : null}
      <h2 className="font-display text-4xl leading-[1.06] tracking-tight text-cream md:text-5xl lg:text-[56px]">
        {title}
      </h2>
      {description ? (
        <p className="max-w-xl text-base leading-relaxed text-muted md:text-lg">{description}</p>
      ) : null}
    </div>
  );
}

/** Soft horizon divider used between sections. */
export function Horizon({ className = "" }: { className?: string }) {
  return <div aria-hidden className={`rule-horizon ${className}`} />;
}
