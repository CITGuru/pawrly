-- 5-backend federated query: TPC-H revenue per nation (in notional USD),
-- enriched with each nation's currency code, then converted to local currency
-- using LIVE FX rates — all five backends joined in one Pawrly plan:
--   facts.lineitem / facts.orders  -> parquet
--   pg.customer                    -> Postgres
--   ref.nation                     -> SQLite
--   world.currency                 -> HTTP  (countriesnow.space)
--   fx.rates                       -> HTTP  (frankfurter.dev, base USD)
WITH revenue AS (             -- parquet + Postgres + SQLite
    SELECT n.n_name AS nation,
           sum(l.l_extendedprice * (1 - l.l_discount)) AS revenue_usd
    FROM facts.lineitem l
    JOIN facts.orders   o ON o.o_orderkey  = l.l_orderkey
    JOIN pg.customer    c ON c.c_custkey   = o.o_custkey
    JOIN ref.nation     n ON n.n_nationkey = c.c_nationkey
    GROUP BY n.n_name
),
fx_long AS (                  -- one fx.rates scan, reshaped to (currency, rate)
    SELECT v.currency,
           CASE v.currency
               WHEN 'USD' THEN 1.0
               WHEN 'EUR' THEN r.eur
               WHEN 'BRL' THEN r.brl
               WHEN 'CAD' THEN r.cad
               WHEN 'CNY' THEN r.cny
               WHEN 'GBP' THEN r.gbp
               WHEN 'INR' THEN r.inr
               WHEN 'IDR' THEN r.idr
               WHEN 'JPY' THEN r.jpy
               WHEN 'RON' THEN r.ron
           END AS rate
    FROM fx.rates r
    CROSS JOIN (VALUES ('USD'),('EUR'),('BRL'),('CAD'),('CNY'),
                       ('GBP'),('INR'),('IDR'),('JPY'),('RON')) AS v(currency)
)
SELECT
    r.nation,
    w.currency,
    round(r.revenue_usd, 2)            AS revenue_usd,
    f.rate                             AS usd_to_local,
    round(r.revenue_usd * f.rate, 2)   AS revenue_local
FROM revenue r
JOIN world.currency w ON upper(w.country) = r.nation   -- HTTP #1
JOIN fx_long        f ON f.currency = w.currency        -- HTTP #2 (live FX)
ORDER BY revenue_usd DESC;
