#!/usr/bin/env python3
"""End-to-end exec benchmark: `pixi exec` vs `rattler exec` vs
`rattler exec --dematerialized` (VFS mount).

Every tool gets its own isolated cache directory, so they never share
packages, repodata or environments. Scenarios:

- warm:  the environment already exists (pixi / rattler: installed prefix;
         --dematerialized: cached lock file, mounted on every run).
- fresh: the environment is deleted before every run, packages and repodata
         stay cached. This is the cost of *spinning up* an environment:
         installing it to disk vs mounting it.
- cold:  the package cache is deleted as well (repodata stays cached), so
         every run downloads its packages.

Only the Python standard library is used, so this runs anywhere a Python 3.9+
interpreter exists (Linux, macOS, Windows).

Example:
    python scripts/bench/vfs_exec_bench.py --rattler target/release/rattler \
        --out bench-results/macos
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, field
from pathlib import Path

READ_ALL_SCRIPT = r'''
import os, sys
files = size = 0
for root, dirs, names in os.walk(sys.prefix):
    for name in names:
        path = os.path.join(root, name)
        if os.path.islink(path) or not os.path.isfile(path):
            continue
        try:
            with open(path, "rb") as f:
                while True:
                    chunk = f.read(1 << 20)
                    if not chunk:
                        break
                    size += len(chunk)
            files += 1
        except OSError:
            pass
print(f"{files} files {size} bytes")
'''


@dataclass
class Workload:
    name: str
    specs: list[str]
    command: list[str]
    description: str


@dataclass
class Tool:
    name: str
    exe: str
    extra: list[str] = field(default_factory=list)
    # Sub-directories of the cache that hold this tool's environments.
    env_dirs: tuple[str, ...] = ()

    def argv(self, workload: Workload, channel: str) -> list[str]:
        argv = [self.exe, "exec", *self.extra, "-c", channel]
        for spec in workload.specs:
            argv += ["-s", spec]
        return [*argv, "--", *workload.command]

    def environ(self, cache: Path) -> dict[str, str]:
        env = dict(os.environ)
        env["RATTLER_CACHE_DIR"] = str(cache)
        env["PIXI_CACHE_DIR"] = str(cache)
        env.setdefault("NO_COLOR", "1")
        return env


def rmtree(path: Path) -> None:
    if not path.exists():
        return
    for attempt in range(5):
        try:
            shutil.rmtree(path)
            return
        except OSError:
            # A just-unmounted mount point (or ProjFS root) can be briefly busy.
            time.sleep(0.5 * (attempt + 1))
    shutil.rmtree(path, ignore_errors=True)


def run_once(argv: list[str], env: dict[str, str], timeout: float) -> float:
    start = time.perf_counter()
    proc = subprocess.run(
        argv,
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        timeout=timeout,
    )
    elapsed = time.perf_counter() - start
    if proc.returncode != 0:
        tail = proc.stderr.decode(errors="replace")[-2000:]
        raise RuntimeError(f"{' '.join(argv)} exited {proc.returncode}:\n{tail}")
    return elapsed


def summarize(samples: list[float]) -> dict[str, float]:
    return {
        "mean": statistics.fmean(samples),
        "median": statistics.median(samples),
        "stddev": statistics.stdev(samples) if len(samples) > 1 else 0.0,
        "min": min(samples),
        "max": max(samples),
        "n": len(samples),
    }


def version_of(exe: str) -> str:
    try:
        out = subprocess.run([exe, "--version"], capture_output=True, text=True, timeout=60)
        return (out.stdout or out.stderr).strip().splitlines()[0]
    except Exception as e:  # noqa: BLE001
        return f"unknown ({e})"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--rattler", required=True, help="path to the rattler binary")
    parser.add_argument("--pixi", default=shutil.which("pixi") or "pixi", help="path to the pixi binary")
    parser.add_argument("--out", type=Path, required=True, help="output directory for results")
    parser.add_argument("--work-dir", type=Path, help="where the isolated caches live (default: a temp dir)")
    parser.add_argument("--channel", default="conda-forge")
    parser.add_argument("--tools", default="pixi,rattler,rattler-vfs")
    parser.add_argument("--scenarios", default="warm,fresh,cold")
    parser.add_argument("--workloads", default="python-startup,import-numpy,read-all")
    parser.add_argument("--runs-warm", type=int, default=10)
    parser.add_argument("--runs-fresh", type=int, default=5)
    parser.add_argument("--runs-cold", type=int, default=3)
    parser.add_argument("--timeout", type=float, default=900)
    args = parser.parse_args()

    args.out.mkdir(parents=True, exist_ok=True)
    work = args.work_dir or Path(tempfile.mkdtemp(prefix="vfs-exec-bench-"))
    work.mkdir(parents=True, exist_ok=True)
    read_all = work / "read_all.py"
    read_all.write_text(READ_ALL_SCRIPT)

    py = ["python=3.12"]
    workloads = {
        w.name: w
        for w in [
            Workload("python-startup", py, ["python", "-c", "pass"], "python -c pass"),
            Workload("import-numpy", [*py, "numpy=2"], ["python", "-c", "import numpy"], "python -c 'import numpy'"),
            Workload("read-all", [*py, "numpy=2"], ["python", str(read_all)], "read every file in the prefix"),
        ]
    }
    rattler = str(Path(args.rattler).resolve())
    tools = {
        t.name: t
        for t in [
            Tool("pixi", args.pixi, env_dirs=("cached-envs-v0", "exec-envs")),
            Tool("rattler", rattler, env_dirs=("exec",)),
            Tool("rattler-vfs", rattler, ["--dematerialized"], env_dirs=("exec",)),
        ]
    }
    runs = {"warm": args.runs_warm, "fresh": args.runs_fresh, "cold": args.runs_cold}

    meta = {
        "os": platform.system(),
        "os_release": platform.release(),
        "machine": platform.machine(),
        "cpu_count": os.cpu_count(),
        "python": sys.version.split()[0],
        "pixi": version_of(args.pixi),
        "rattler": version_of(rattler),
        "channel": args.channel,
        "started": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "workloads": {w.name: {"specs": w.specs, "description": w.description} for w in workloads.values()},
    }
    print(json.dumps(meta, indent=2), flush=True)

    results: list[dict] = []
    for wname in args.workloads.split(","):
        workload = workloads[wname]
        for tname in args.tools.split(","):
            tool = tools[tname]
            cache = work / "caches" / tname
            env = tool.environ(cache)
            argv = tool.argv(workload, args.channel)

            def drop_envs() -> None:
                for d in tool.env_dirs:
                    rmtree(cache / d)

            for scenario in args.scenarios.split(","):
                n = runs[scenario]
                if n <= 0:
                    continue
                label = f"{wname:<15} {tname:<12} {scenario:<6}"
                try:
                    # Prime caches (repodata always; packages for warm/fresh;
                    # the environment itself for warm).
                    run_once(argv, env, args.timeout)
                    samples = []
                    for _ in range(n):
                        if scenario in ("fresh", "cold"):
                            drop_envs()
                        if scenario == "cold":
                            rmtree(cache / "pkgs")
                        samples.append(run_once(argv, env, args.timeout))
                except Exception as e:  # noqa: BLE001
                    print(f"{label} FAILED: {e}", flush=True)
                    results.append({"workload": wname, "tool": tname, "scenario": scenario, "error": str(e)})
                    continue
                stats = summarize(samples)
                print(
                    f"{label} {stats['mean'] * 1000:9.1f} ms ± {stats['stddev'] * 1000:7.1f}"
                    f"  (median {stats['median'] * 1000:.1f}, n={n})",
                    flush=True,
                )
                results.append(
                    {"workload": wname, "tool": tname, "scenario": scenario, "samples": samples, **stats}
                )
            # Leave a clean slate (and no mounts) for the next tool.
            drop_envs()

    out = {"meta": meta, "results": results}
    (args.out / "exec-results.json").write_text(json.dumps(out, indent=2))
    print(f"wrote {args.out / 'exec-results.json'}")
    return 0 if all("error" not in r for r in results) else 1


if __name__ == "__main__":
    sys.exit(main())
