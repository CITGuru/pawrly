#!/usr/bin/env python3
# Performance harness for the pawrly CLI: times the materialize lifecycle
# (file / query / namespaced origins, read-back, full-row fetch) across
# dataset sizes and prints median wall-clock seconds per operation.
#
# Every number includes one full CLI invocation, so the reported baseline
# (`sql 'SELECT 1'`) is the per-call floor to subtract mentally. Runs against
# an isolated PAWRLY_HOME inside the work dir — your real cache is untouched.
#
# Usage:
#   cargo build --release -p pawrly-cli
#   scripts/perf.py                            # in-process, 10k/100k/1M rows
#   scripts/perf.py --sizes 10000,100000       # custom sizes
#   scripts/perf.py --remote tcp://127.0.0.1:50051   # against a running daemon
#   scripts/perf.py --bin target/debug/pawrly  # a specific binary
#   scripts/perf.py --keep                     # keep the work dir for reruns
"""Materialize performance harness. See the header comment for usage."""

import argparse
import csv
import os
import random
import shutil
import statistics
import subprocess
import sys
import tempfile
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

CONFIG_TEMPLATE = """version: 1
defaults:
  cache:
    storage: "./.pawrly/cache"
    namespace: perfbench
sources:
  - name: data
    kind: file
    config:
      path: "."
    tables:
{tables}
"""


def generate(workdir: str, sizes: list[int]) -> None:
    random.seed(42)
    regions = ["north", "south", "east", "west", "central"]
    products = ["widget", "gadget", "doohickey", "gizmo", "sprocket", "cog"]
    tables = []
    for n in sizes:
        path = os.path.join(workdir, f"orders_{n}.csv")
        if not os.path.exists(path):
            with open(path, "w", newline="") as f:
                w = csv.writer(f)
                w.writerow(["id", "region", "product", "amount", "ts"])
                for i in range(n):
                    w.writerow([
                        i,
                        random.choice(regions),
                        random.choice(products),
                        round(random.uniform(1, 5000), 2),
                        f"2026-{random.randint(1, 12):02d}-{random.randint(1, 28):02d}",
                    ])
        tables.append(
            f"      - name: orders_{tag(n)}\n"
            f"        path: \"./orders_{n}.csv\"\n"
            f"        format: csv"
        )
    with open(os.path.join(workdir, "pawrly.yaml"), "w") as f:
        f.write(CONFIG_TEMPLATE.format(tables="\n".join(tables)))


def tag(n: int) -> str:
    if n % 1_000_000 == 0:
        return f"{n // 1_000_000}m"
    if n % 1_000 == 0:
        return f"{n // 1_000}k"
    return str(n)


class Bench:
    def __init__(self, binary: str, workdir: str, remote: str | None, reps: int):
        self.base = [binary, "--config", os.path.join(workdir, "pawrly.yaml")]
        if remote:
            self.base += ["--remote", remote]
        self.env = {**os.environ, "PAWRLY_HOME": os.path.join(workdir, ".home")}
        self.reps = reps

    def run_once(self, args: list[str]) -> float:
        t0 = time.perf_counter()
        r = subprocess.run(self.base + args, env=self.env, capture_output=True, text=True)
        dt = time.perf_counter() - t0
        if r.returncode != 0:
            sys.exit(f"FAILED {' '.join(args)}\n{r.stderr[:500]}")
        return dt

    def run(self, label: str, args: list[str]) -> float:
        times = [self.run_once(args) for _ in range(self.reps)]
        med = statistics.median(times)
        print(f"  {label:<46} {med:7.3f}s  (min {min(times):.3f}s)")
        return med


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--bin", default=os.path.join(REPO, "target", "release", "pawrly"))
    ap.add_argument("--remote", default=None, help="daemon endpoint (e.g. tcp://127.0.0.1:50051)")
    ap.add_argument("--sizes", default="10000,100000,1000000")
    ap.add_argument("--reps", type=int, default=3)
    ap.add_argument("--dir", default=None, help="work dir (default: a temp dir)")
    ap.add_argument("--keep", action="store_true", help="keep the work dir")
    args = ap.parse_args()

    if not os.path.exists(args.bin):
        sys.exit(f"binary not found: {args.bin}\nbuild it first: cargo build --release -p pawrly-cli")
    sizes = [int(s) for s in args.sizes.split(",")]

    workdir = args.dir or tempfile.mkdtemp(prefix="pawrly-perf-")
    os.makedirs(workdir, exist_ok=True)
    try:
        print(f"work dir: {workdir}")
        generate(workdir, sizes)
        b = Bench(args.bin, workdir, args.remote, args.reps)

        print(f"mode: {'daemon ' + args.remote if args.remote else 'in-process'}")
        base = b.run("baseline: sql 'SELECT 1' (per-call floor)", ["sql", "SELECT 1"])

        for n in sizes:
            t = tag(n)
            csv_path = os.path.join(workdir, f"orders_{n}.csv")
            print(f"\n--- {t} rows ---")
            b.run(f"file materialize ({t} CSV)", ["materialize", f"raw_{t}", "--file", csv_path])
            b.run(f"query materialize: full copy ({t})", ["materialize", f"copy_{t}", f"SELECT * FROM data.orders_{t}"])
            b.run(
                f"query materialize: aggregation ({t})",
                ["materialize", f"agg_{t}",
                 f"SELECT region, product, SUM(amount) AS total, COUNT(*) AS n FROM data.orders_{t} GROUP BY 1, 2"],
            )
            b.run(
                f"namespaced file materialize ({t})",
                ["materialize", f"raw_{t}", "--file", csv_path, "--namespace", "perf"],
            )
            b.run(f"readback: COUNT(*) ({t})", ["sql", f"SELECT COUNT(*) AS n FROM materialized.copy_{t}"])
            b.run(
                f"readback: aggregate ({t})",
                ["sql", f"SELECT region, SUM(amount) AS s FROM materialized.copy_{t} GROUP BY 1"],
            )
            b.run(
                f"full-row fetch to csv ({t})",
                ["sql", f"SELECT * FROM materialized.copy_{t}", "--format", "csv"],
            )

        print(f"\n(baseline {base:.3f}s of every number above is per-call overhead, not data work)")
    finally:
        if not args.keep and args.dir is None:
            shutil.rmtree(workdir, ignore_errors=True)
        elif args.keep:
            print(f"kept work dir: {workdir}")


if __name__ == "__main__":
    main()
