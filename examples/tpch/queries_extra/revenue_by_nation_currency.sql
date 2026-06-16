-- 4-backend federated query: revenue per customer nation, enriched with each
-- nation's live currency code from a public HTTP API.
--   facts.lineitem / facts.orders  -> parquet
--   pg.customer                    -> Postgres
--   ref.nation                     -> SQLite
--   world.currency                 -> HTTP (countriesnow.space)
SELECT
    n.n_name                                            AS nation,
    w.currency,
    w.iso3,
    round(sum(l.l_extendedprice * (1 - l.l_discount)), 2) AS revenue
FROM facts.lineitem l
JOIN facts.orders   o ON o.o_orderkey  = l.l_orderkey
JOIN pg.customer    c ON c.c_custkey   = o.o_custkey
JOIN ref.nation     n ON n.n_nationkey = c.c_nationkey
JOIN world.currency w ON upper(w.country) = n.n_name
GROUP BY n.n_name, w.currency, w.iso3
ORDER BY revenue DESC
LIMIT 10;
