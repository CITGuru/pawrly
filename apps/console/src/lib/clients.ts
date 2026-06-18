import {
  createClient,
  type Client,
  type Interceptor,
} from "@connectrpc/connect";
import { createGrpcWebTransport } from "@connectrpc/connect-web";

import { AdminService } from "@/gen/pawrly/v1/admin_pb";
import { QueryService } from "@/gen/pawrly/v1/query_pb";
import { CatalogService } from "@/gen/pawrly/v1/catalog_pb";
import { SourcesService } from "@/gen/pawrly/v1/sources_pb";
import { CacheService } from "@/gen/pawrly/v1/cache_pb";
import { SemanticService } from "@/gen/pawrly/v1/semantic_pb";

export interface Clients {
  admin: Client<typeof AdminService>;
  query: Client<typeof QueryService>;
  catalog: Client<typeof CatalogService>;
  sources: Client<typeof SourcesService>;
  cache: Client<typeof CacheService>;
  semantic: Client<typeof SemanticService>;
}

/**
 * Build the gRPC-Web clients for a daemon at `baseUrl`. The bearer token (when
 * set) rides as `Authorization` metadata on every call — the same token the
 * daemon's `AuthInterceptor` validates.
 */
function randomHex(bytes: number): string {
  const arr = new Uint8Array(bytes);
  crypto.getRandomValues(arr);
  return Array.from(arr, (b) => b.toString(16).padStart(2, "0")).join("");
}

export function createClients(baseUrl: string, token: string): Clients {
  const auth: Interceptor = (next) => (req) => {
    if (token) {
      req.header.set("Authorization", `Bearer ${token}`);
    }
    return next(req);
  };
  // Emit a fresh W3C traceparent per call so each operation is recorded with a
  // real trace_id in system.activity (and is deep-linkable to a trace backend).
  const trace: Interceptor = (next) => (req) => {
    req.header.set("traceparent", `00-${randomHex(16)}-${randomHex(8)}-01`);
    return next(req);
  };
  const transport = createGrpcWebTransport({
    baseUrl,
    interceptors: [trace, auth],
  });
  return {
    admin: createClient(AdminService, transport),
    query: createClient(QueryService, transport),
    catalog: createClient(CatalogService, transport),
    sources: createClient(SourcesService, transport),
    cache: createClient(CacheService, transport),
    semantic: createClient(SemanticService, transport),
  };
}
