import { DocsNav } from "./DocsNav";

/** Desktop-only sidebar rail. Mobile nav lives in the header hamburger drawer. */
export function DocsSidebar() {
  return (
    <aside className="hidden lg:block">
      <div className="sticky top-20 max-h-[calc(100vh-6rem)] overflow-y-auto pb-8 pr-2">
        <DocsNav />
      </div>
    </aside>
  );
}
