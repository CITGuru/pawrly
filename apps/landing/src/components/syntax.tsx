import { Fragment, type ReactNode } from "react";

export type Lang = "sql" | "yaml" | "bash";

type Rule = { re: RegExp; cls: string };

// Lightweight, display-only tokenizers. Not a real parser — just enough to give
// the marketing code a calm, legible tint. Rules are tried in order at each
// cursor position; first match at the cursor wins.
const RULES: Record<Lang, Rule[]> = {
  bash: [
    { re: /#.*/y, cls: "tok-com" },
    { re: /"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'/y, cls: "tok-str" },
    { re: /\$\{[^}]*\}|\$[A-Za-z_][A-Za-z0-9_]*/y, cls: "tok-num" },
    {
      re: /\b(pawrly|curl|export|sh|sql|serve|status|schema|validate|mcp-stdio)\b/y,
      cls: "tok-fn",
    },
  ],
  sql: [
    { re: /--.*/y, cls: "tok-com" },
    { re: /'(?:[^'\\]|\\.)*'/y, cls: "tok-str" },
    { re: /\b\d+(?:\.\d+)?\b/y, cls: "tok-num" },
    {
      re: /\b(SELECT|FROM|JOIN|LEFT|RIGHT|INNER|OUTER|ON|WHERE|GROUP|BY|ORDER|ASC|DESC|AND|OR|NOT|AS|BETWEEN|COUNT|SUM|AVG|MIN|MAX|INTERVAL|LIMIT|HAVING|DISTINCT|true|false|null)\b/iy,
      cls: "tok-key",
    },
    { re: /\b[a-z_][a-z0-9_]*(?=\.)/iy, cls: "tok-fn" },
  ],
  yaml: [
    { re: /#.*/y, cls: "tok-com" },
    { re: /"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'/y, cls: "tok-str" },
    { re: /\$\{[^}]*\}/y, cls: "tok-num" },
    { re: /[A-Za-z0-9_.$-]+(?=\s*:)/y, cls: "tok-key" },
    { re: /\b\d+(?:\.\d+)?\b/y, cls: "tok-num" },
  ],
};

function tokenizeLine(line: string, lang: Lang): ReactNode[] {
  const rules = RULES[lang];
  const out: ReactNode[] = [];
  let i = 0;
  let plain = "";
  let k = 0;
  const flush = () => {
    if (plain) {
      out.push(<Fragment key={`p${k++}`}>{plain}</Fragment>);
      plain = "";
    }
  };
  while (i < line.length) {
    let matched = false;
    for (const { re, cls } of rules) {
      re.lastIndex = i;
      const m = re.exec(line);
      if (m && m.index === i && m[0].length > 0) {
        flush();
        out.push(
          <span key={`t${k++}`} className={cls}>
            {m[0]}
          </span>
        );
        i += m[0].length;
        matched = true;
        break;
      }
    }
    if (!matched) {
      plain += line[i];
      i += 1;
    }
  }
  flush();
  return out;
}

/** Just the highlighted <pre><code> — no window chrome. */
export function CodeLines({
  lang,
  code,
  className = "",
}: {
  lang: Lang;
  code: string;
  className?: string;
}) {
  const lines = code.replace(/\n$/, "").split("\n");
  return (
    <pre className={`overflow-x-auto font-mono ${className}`}>
      <code>
        {lines.map((line, idx) => (
          <div key={idx} className="whitespace-pre">
            {line.length ? tokenizeLine(line, lang) : " "}
          </div>
        ))}
      </code>
    </pre>
  );
}

/** The three macOS-style window dots. */
export function WindowDots() {
  return (
    <span className="flex gap-1.5">
      <span className="h-2.5 w-2.5 rounded-full bg-coral/70" />
      <span className="h-2.5 w-2.5 rounded-full bg-gold/70" />
      <span className="h-2.5 w-2.5 rounded-full bg-foam/40" />
    </span>
  );
}
