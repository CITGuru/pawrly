/**
 * Full-viewport film grain. Fixed so it stays put as you scroll, blended soft
 * over the water so the page never looks flatly digital. Pointer-events off.
 */
export function Grain() {
  return (
    <div
      aria-hidden
      className="grain pointer-events-none fixed inset-0 z-[100] opacity-[0.045] mix-blend-soft-light"
    />
  );
}
