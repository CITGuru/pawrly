import { Mark } from "./Mark";

export function Logo({ className = "" }: { className?: string }) {
  return (
    <span className={`inline-flex items-center gap-2.5 text-cream ${className}`}>
      <Mark size={26} carve="var(--ocean-900)" />
      <span className="text-[19px] font-semibold tracking-tight text-cream">pawrly</span>
    </span>
  );
}
