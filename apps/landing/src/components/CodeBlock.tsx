import { CodeLines, WindowDots, type Lang } from "./syntax";

export function CodeBlock({
  lang,
  code,
  title,
  className = "",
  bodyClassName = "",
}: {
  lang: Lang;
  code: string;
  title?: string;
  className?: string;
  /** Extra classes on the scrollable code body — e.g. a max-height to cap a long file. */
  bodyClassName?: string;
}) {
  return (
    <div className={`code-surface overflow-hidden rounded-2xl soft-shadow ${className}`}>
      <div className="flex items-center gap-2 border-b border-line px-4 py-3">
        <WindowDots />
        {title ? (
          <span className="ml-2 font-mono text-xs text-muted-2">{title}</span>
        ) : null}
      </div>
      <CodeLines
        lang={lang}
        code={code}
        className={`px-4 py-4 text-[13px] leading-[1.65] text-cream ${bodyClassName}`}
      />
    </div>
  );
}
