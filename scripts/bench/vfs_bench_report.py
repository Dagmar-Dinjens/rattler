#!/usr/bin/env python3
"""Collect and summarize the rattler_vfs benchmark results.

    # Flatten criterion's target/criterion tree into one JSON file.
    vfs_bench_report.py collect-criterion target/criterion out/criterion-results.json

    # Merge per-OS result directories (each holding exec-results.json and/or
    # criterion-results.json) into a markdown report on stdout.
    vfs_bench_report.py report results/linux results/macos results/windows
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

TOOLS = ["pixi", "rattler", "rattler-vfs"]
TOOL_LABELS = {
    "pixi": "pixi exec",
    "rattler": "rattler exec",
    "rattler-vfs": "rattler exec --dematerialized",
}


def collect_criterion(criterion_dir: Path, out: Path) -> None:
    rows = []
    for bench in sorted(criterion_dir.rglob("new/benchmark.json")):
        estimates = bench.parent / "estimates.json"
        if not estimates.exists():
            continue
        info = json.loads(bench.read_text())
        est = json.loads(estimates.read_text())
        row = {
            "id": info["full_id"],
            "mean_ns": est["mean"]["point_estimate"],
            "mean_lo_ns": est["mean"]["confidence_interval"]["lower_bound"],
            "mean_hi_ns": est["mean"]["confidence_interval"]["upper_bound"],
            "median_ns": est["median"]["point_estimate"],
            "stddev_ns": est["std_dev"]["point_estimate"],
        }
        throughput = info.get("throughput") or {}
        if "Bytes" in throughput:
            row["bytes"] = throughput["Bytes"]
        if "Elements" in throughput:
            row["elements"] = throughput["Elements"]
        rows.append(row)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(rows, indent=2))
    print(f"collected {len(rows)} criterion benchmarks into {out}")


def fmt_time(seconds: float) -> str:
    if seconds < 0:
        return "-" + fmt_time(-seconds)
    if seconds >= 1:
        return f"{seconds:.2f} s"
    if seconds >= 1e-3:
        return f"{seconds * 1e3:.1f} ms"
    if seconds >= 1e-6:
        return f"{seconds * 1e6:.1f} µs"
    return f"{seconds * 1e9:.0f} ns"


def os_name(d: Path, meta: dict | None) -> str:
    if meta:
        return f"{meta['os']} {meta['machine']}"
    return d.name


def exec_tables(dirs: list[Path]) -> list[str]:
    lines = []
    for d in dirs:
        path = d / "exec-results.json"
        if not path.exists():
            continue
        data = json.loads(path.read_text())
        meta = data["meta"]
        lines += [
            f"### {os_name(d, meta)}",
            "",
            f"{meta['pixi']} · {meta['rattler']} · {meta['cpu_count']} CPUs · channel `{meta['channel']}`",
            "",
            "| workload | scenario | " + " | ".join(TOOL_LABELS[t] for t in TOOLS) + " | VFS vs pixi | VFS vs rattler |",
            "|---|---|" + "---:|" * (len(TOOLS) + 2),
        ]
        by_key = {(r["workload"], r["scenario"], r["tool"]): r for r in data["results"]}
        workloads = list(dict.fromkeys(r["workload"] for r in data["results"]))
        scenarios = list(dict.fromkeys(r["scenario"] for r in data["results"]))
        for w in workloads:
            for s in scenarios:
                cells, means = [], {}
                for t in TOOLS:
                    r = by_key.get((w, s, t))
                    if r is None:
                        cells.append("–")
                    elif "error" in r:
                        cells.append("failed")
                    else:
                        means[t] = r["mean"]
                        cells.append(f"{fmt_time(r['mean'])} ± {fmt_time(r['stddev'])}")
                ratios = []
                for base in ("pixi", "rattler"):
                    if base in means and "rattler-vfs" in means:
                        ratio = means[base] / means["rattler-vfs"]
                        ratios.append(f"{ratio:.2f}× {'faster' if ratio >= 1 else 'slower'}")
                    else:
                        ratios.append("–")
                lines.append(f"| {w} | {s} | " + " | ".join(cells + ratios) + " |")
        lines += ["", *spinup_lines(by_key, workloads), ""]
    return lines


def spinup_lines(by_key: dict, workloads: list[str]) -> list[str]:
    """Env spin-up cost (fresh − warm) and the run count where VFS stops paying off."""
    lines = [
        "Spin-up cost (fresh − warm) and break-even: VFS is cheaper until an environment",
        "has been used this many times (`1 + (fresh_disk − fresh_vfs) / (warm_vfs − warm_disk)`).",
        "",
        "| workload | " + " | ".join(f"spin-up {TOOL_LABELS[t]}" for t in TOOLS) + " | break-even vs pixi | break-even vs rattler |",
        "|---|" + "---:|" * (len(TOOLS) + 2),
    ]
    for w in workloads:
        cells = []
        get = lambda t, s: by_key.get((w, s, t), {}).get("mean")  # noqa: E731
        for t in TOOLS:
            fresh, warm = get(t, "fresh"), get(t, "warm")
            cells.append(fmt_time(fresh - warm) if fresh is not None and warm is not None else "–")
        for base in ("pixi", "rattler"):
            fd, fv, wd, wv = get(base, "fresh"), get("rattler-vfs", "fresh"), get(base, "warm"), get("rattler-vfs", "warm")
            if None in (fd, fv, wd, wv):
                cells.append("–")
            elif fv >= fd:
                cells.append("never (VFS fresh is slower)")
            elif wv <= wd:
                cells.append("always (VFS warm is faster)")
            else:
                cells.append(f"{1 + (fd - fv) / (wv - wd):.1f} runs")
        lines.append(f"| {w} | " + " | ".join(cells) + " |")
    return lines


def criterion_table(dirs: list[Path]) -> list[str]:
    per_os: dict[str, dict[str, dict]] = {}
    for d in dirs:
        path = d / "criterion-results.json"
        if not path.exists():
            continue
        meta_path = d / "exec-results.json"
        meta = json.loads(meta_path.read_text())["meta"] if meta_path.exists() else None
        per_os[os_name(d, meta)] = {r["id"]: r for r in json.loads(path.read_text())}
    if not per_os:
        return []
    ids = sorted({i for rows in per_os.values() for i in rows})
    names = list(per_os)
    lines = [
        "## rattler_vfs crate benchmarks (criterion, mean)",
        "",
        "| benchmark | " + " | ".join(names) + " |",
        "|---|" + "---:|" * len(names),
    ]
    for i in ids:
        cells = []
        for n in names:
            r = per_os[n].get(i)
            if r is None:
                cells.append("–")
                continue
            secs = r["mean_ns"] / 1e9
            cell = fmt_time(secs)
            if "bytes" in r and secs > 0:
                cell += f" ({r['bytes'] / secs / 2**20:,.0f} MiB/s)"
            cells.append(cell)
        lines.append(f"| `{i}` | " + " | ".join(cells) + " |")
    return lines


def report(dirs: list[Path]) -> None:
    lines = ["# rattler_vfs benchmarks", "", "## exec: pixi vs rattler vs rattler --dematerialized", ""]
    lines += exec_tables(dirs)
    lines += criterion_table(dirs)
    print("\n".join(lines))


def main() -> int:
    if len(sys.argv) >= 4 and sys.argv[1] == "collect-criterion":
        collect_criterion(Path(sys.argv[2]), Path(sys.argv[3]))
        return 0
    if len(sys.argv) >= 3 and sys.argv[1] == "report":
        report([Path(p) for p in sys.argv[2:]])
        return 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main())
