import { createContext, useContext } from "react";

import type { PageId } from "@/components/Sidebar";

export interface NavValue {
  navigate: (page: PageId) => void;
  openSql: (sql: string) => void;
  /** SQL queued by `openSql`; the runner reads it on mount, then clears it. */
  pendingSql: string | null;
  consumePendingSql: () => void;
}

export const NavContext = createContext<NavValue | null>(null);

export function useNav(): NavValue {
  const value = useContext(NavContext);
  if (!value) {
    throw new Error("useNav must be used within a NavContext provider");
  }
  return value;
}
