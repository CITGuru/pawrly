import {
  createContext,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";

import { createClients, type Clients } from "./clients";

const TRACE_TEMPLATE_KEY = "pawrly.console.traceUrlTemplate";

interface ConnectionValue {
  baseUrl: string;
  token: string;
  /** URL template for trace deep links, with a `{traceId}` placeholder. */
  traceUrlTemplate: string;
  setBaseUrl: (url: string) => void;
  setToken: (token: string) => void;
  setTraceUrlTemplate: (tpl: string) => void;
  clients: Clients;
}

const ConnectionContext = createContext<ConnectionValue | null>(null);

export function ConnectionProvider({ children }: { children: ReactNode }) {
  // Embedded same-origin default; the daemon serving this SPA is the daemon we
  // talk to. A standalone bundle can override via /config.json or the UI field.
  const [baseUrl, setBaseUrl] = useState(() => window.location.origin);
  const [token, setToken] = useState("");
  // Persisted (non-secret) so a deep-link target survives reloads.
  const [traceUrlTemplate, setTraceTemplateState] = useState(
    () => localStorage.getItem(TRACE_TEMPLATE_KEY) ?? "",
  );

  function setTraceUrlTemplate(tpl: string) {
    setTraceTemplateState(tpl);
    if (tpl) localStorage.setItem(TRACE_TEMPLATE_KEY, tpl);
    else localStorage.removeItem(TRACE_TEMPLATE_KEY);
  }

  useEffect(() => {
    let cancelled = false;
    void fetch("/config.json")
      .then((r) => (r.ok ? r.json() : null))
      .then((cfg: unknown) => {
        if (cancelled || !cfg || typeof cfg !== "object") return;
        const c = cfg as { baseUrl?: unknown; traceUrlTemplate?: unknown };
        if (typeof c.baseUrl === "string" && c.baseUrl) setBaseUrl(c.baseUrl);
        // config.json supplies a default only if the user hasn't set one.
        if (
          typeof c.traceUrlTemplate === "string" &&
          c.traceUrlTemplate &&
          !localStorage.getItem(TRACE_TEMPLATE_KEY)
        ) {
          setTraceTemplateState(c.traceUrlTemplate);
        }
      })
      .catch(() => {
        /* no standalone config — stay on same-origin */
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const clients = useMemo(() => createClients(baseUrl, token), [baseUrl, token]);

  const value = useMemo<ConnectionValue>(
    () => ({
      baseUrl,
      token,
      traceUrlTemplate,
      setBaseUrl,
      setToken,
      setTraceUrlTemplate,
      clients,
    }),
    [baseUrl, token, traceUrlTemplate, clients],
  );

  return (
    <ConnectionContext.Provider value={value}>
      {children}
    </ConnectionContext.Provider>
  );
}

export function useConnection(): ConnectionValue {
  const value = useContext(ConnectionContext);
  if (!value) {
    throw new Error("useConnection must be used within a ConnectionProvider");
  }
  return value;
}

export function useClients(): Clients {
  return useConnection().clients;
}
