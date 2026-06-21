import { CodeLines, WindowDots } from "./syntax";

// A representative cross-source join matching the source spec beside it: paying
// Stripe customers that Intercom hasn't seen in a while — two APIs, one
// statement, with a rendered result + meta.
const SQL = `SELECT c.email, c.name, i.last_seen_at
FROM stripe.customers c
JOIN intercom.contacts i
  ON i.email = c.email
WHERE c.delinquent = false
ORDER BY i.last_seen_at ASC;`;

const ROWS = [
  { email: "ava@northwind.co", name: "Ava Chen", seen: "41 days ago" },
  { email: "sam@globex.io", name: "Sam Okoye", seen: "33 days ago" },
  { email: "lin@initech.dev", name: "Lin Park", seen: "29 days ago" },
];

export function QueryCard({ className = "" }: { className?: string }) {
  return (
    <div className={`code-surface overflow-hidden rounded-2xl soft-shadow ${className}`}>
      <div className="flex items-center gap-2 border-b border-line px-4 py-3">
        <WindowDots />
        <span className="ml-2 font-mono text-xs text-muted-2">pawrly sql</span>
      </div>

      <div className="px-5 py-5">
        <CodeLines lang="sql" code={SQL} className="text-[13px] leading-[1.7] text-cream" />

        {/* Result set */}
        <div className="mt-6 overflow-x-auto">
          <table className="w-full border-collapse font-mono text-[12.5px]">
            <colgroup>
              <col style={{ width: "42%" }} />
              <col style={{ width: "28%" }} />
              <col style={{ width: "30%" }} />
            </colgroup>
            <thead>
              <tr className="border-b border-line text-muted-2">
                <th className="py-2 text-left font-normal">email</th>
                <th className="py-2 text-left font-normal">name</th>
                <th className="py-2 text-left font-normal">last_seen_at</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-line">
              {ROWS.map((r) => (
                <tr key={r.email}>
                  <td className="py-2.5 pr-4 text-[#9ed0f0]">{r.email}</td>
                  <td className="py-2.5 pr-4 text-cream">{r.name}</td>
                  <td className="py-2.5 text-foam">{r.seen}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>

        {/* Meta — styled like trailing SQL comments */}
        <div className="mt-5 space-y-1 font-mono text-[12px] leading-relaxed text-muted-2">
          <p>— Stripe × Intercom · 3 rows · 84ms</p>
          <p>— semantic hints applied · hot path cached</p>
        </div>
      </div>
    </div>
  );
}
