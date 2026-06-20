import { PawMark } from "./PawMark";
import { PalmMark } from "./PalmMark";

/**
 * Renders both marks; CSS (driven by <html data-mark>) shows exactly one.
 * Lets the live design switcher flip paw <-> palm with zero prop-drilling.
 * Static build-time art (favicon, OG, apple-icon) stays on the paw until a
 * direction is chosen.
 */
type MarkProps = {
  className?: string;
  size?: number;
  pads?: string;
  carve?: string;
};

export function Mark(props: MarkProps) {
  return (
    <>
      <span className="mark-paw">
        <PawMark {...props} />
      </span>
      <span className="mark-palm">
        <PalmMark {...props} />
      </span>
    </>
  );
}
