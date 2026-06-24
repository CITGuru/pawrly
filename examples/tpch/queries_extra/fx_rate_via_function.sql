-- Federated query using a table-valued FUNCTION: each TPC-H nation's currency
-- (SQLite + HTTP table), priced against live FX rates pulled through a
-- parameterized HTTP FUNCTION whose base currency is the call argument.
--   ref.nation         -> SQLite
--   world.currency      -> HTTP     (countriesnow.space)
--   fx.rates_for(base)  -> HTTP function (frankfurter.dev; base = the argument)
SELECT n.n_name             AS nation,
       w.currency,
       r.usd                AS usd_per_eur,
       r.jpy                AS jpy_per_eur
FROM ref.nation n
JOIN world.currency w   ON upper(w.country) = n.n_name
CROSS JOIN fx.rates_for('EUR') r          -- one HTTP function call, base = EUR
ORDER BY n.n_name
LIMIT 10;
