#!/usr/bin/env python3
"""Benchmark cached language-image startup against the diagnostic Docker guest."""

from __future__ import annotations

import argparse
import json
import math
import os
import statistics
import subprocess
import sys
import time
import uuid
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable, Sequence


@dataclass(frozen=True)
class RuntimeCase:
    name: str
    image: str
    entrypoint: str
    arguments: tuple[str, ...]


CASES = (
    RuntimeCase("java", "eclipse-temurin:21-jdk-alpine", "java", ("-version",)),
    RuntimeCase("python", "python:3.13-alpine", "python", ("-c", "pass")),
    RuntimeCase("go", "golang:1.25-alpine", "go", ("version",)),
    RuntimeCase("rust", "rust:1-alpine", "rustc", ("--version",)),
    RuntimeCase("nodejs", "node:22-alpine", "node", ("-e", "")),
    RuntimeCase("bun", "oven/bun:1-alpine", "bun", ("-e", "")),
)


class BenchmarkError(RuntimeError):
    pass


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--docker-host",
        default=os.environ.get("DOCKER_HOST", "tcp://127.0.0.1:12375"),
    )
    parser.add_argument("--runs", type=positive_int, default=10)
    parser.add_argument("--warmups", type=non_negative_int, default=2)
    parser.add_argument(
        "--settle-ms",
        type=non_negative_float,
        default=100.0,
        help="unmeasured delay after each sample so async cleanup can settle",
    )
    parser.add_argument(
        "--runtime",
        action="append",
        choices=tuple(case.name for case in CASES),
        help="benchmark only this runtime; repeat to select more than one",
    )
    parser.add_argument(
        "--extended",
        action="store_true",
        help="also benchmark prepared restart, start-to-running, and warm exec",
    )
    parser.add_argument(
        "--warm-exec",
        action="store_true",
        help="also benchmark commands inside one already-running container",
    )
    parser.add_argument("--output", type=Path)
    return parser.parse_args()


def positive_int(raw: str) -> int:
    value = int(raw)
    if value <= 0:
        raise argparse.ArgumentTypeError("must be greater than zero")
    return value


def non_negative_int(raw: str) -> int:
    value = int(raw)
    if value < 0:
        raise argparse.ArgumentTypeError("must not be negative")
    return value


def non_negative_float(raw: str) -> float:
    value = float(raw)
    if value < 0:
        raise argparse.ArgumentTypeError("must not be negative")
    return value


def docker_command(host: str, *arguments: str) -> list[str]:
    return ["docker", "--host", host, *arguments]


def run_checked(command: Sequence[str], *, capture: bool = False) -> str:
    completed = subprocess.run(
        command,
        check=False,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE if capture else subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
        timeout=120,
    )
    if completed.returncode != 0:
        detail = completed.stderr.strip() or f"exit {completed.returncode}"
        raise BenchmarkError(f"{' '.join(command)}: {detail}")
    return completed.stdout.strip() if capture else ""


def elapsed_ms(command: Sequence[str]) -> float:
    started = time.perf_counter_ns()
    run_checked(command)
    return (time.perf_counter_ns() - started) / 1_000_000


def summarize(first_ms: float, samples_ms: list[float]) -> dict[str, Any]:
    ordered = sorted(samples_ms)
    p95_index = max(0, math.ceil(len(ordered) * 0.95) - 1)
    return {
        "first_ms": round(first_ms, 3),
        "median_ms": round(statistics.median(ordered), 3),
        "p95_ms": round(ordered[p95_index], 3),
        "min_ms": round(ordered[0], 3),
        "max_ms": round(ordered[-1], 3),
        "samples_ms": [round(sample, 3) for sample in samples_ms],
    }


def benchmark_command(
    command: Sequence[str], *, warmups: int, runs: int, settle_seconds: float
) -> dict[str, Any]:
    return benchmark_operation(
        lambda: elapsed_ms(command),
        warmups=warmups,
        runs=runs,
        settle_seconds=settle_seconds,
    )


def benchmark_operation(
    operation: Callable[[], float],
    *,
    warmups: int,
    runs: int,
    settle_seconds: float,
) -> dict[str, Any]:
    def sample() -> float:
        measured = operation()
        if settle_seconds:
            time.sleep(settle_seconds)
        return measured

    first_ms = sample()
    for _ in range(warmups):
        sample()
    samples_ms = [sample() for _ in range(runs)]
    return summarize(first_ms, samples_ms)


def run_command(host: str, case: RuntimeCase, *, lifecycle_only: bool) -> list[str]:
    entrypoint = "/bin/true" if lifecycle_only else case.entrypoint
    arguments: tuple[str, ...] = () if lifecycle_only else case.arguments
    return docker_command(
        host,
        "run",
        "--rm",
        "--pull",
        "never",
        "--network",
        "host",
        "--platform",
        "linux/amd64",
        "--entrypoint",
        entrypoint,
        case.image,
        *arguments,
    )


def benchmark_prepared_start(
    host: str,
    case: RuntimeCase,
    *,
    warmups: int,
    runs: int,
    settle_seconds: float,
) -> tuple[dict[str, Any], float, float]:
    name = f"rish-bench-{case.name}-{uuid.uuid4().hex[:10]}"
    create = docker_command(
        host,
        "create",
        "--pull",
        "never",
        "--name",
        name,
        "--network",
        "host",
        "--platform",
        "linux/amd64",
        "--entrypoint",
        case.entrypoint,
        case.image,
        *case.arguments,
    )
    start = docker_command(host, "start", "--attach", name)
    remove = docker_command(host, "rm", name)
    create_ms = elapsed_ms(create)
    try:
        result = benchmark_command(
            start,
            warmups=warmups,
            runs=runs,
            settle_seconds=settle_seconds,
        )
    finally:
        remove_ms = elapsed_ms(remove)
    return result, create_ms, remove_ms


def benchmark_keepalive(
    host: str,
    case: RuntimeCase,
    *,
    warmups: int,
    runs: int,
    settle_seconds: float,
) -> tuple[dict[str, Any], dict[str, Any], float, float]:
    name = f"rish-bench-live-{case.name}-{uuid.uuid4().hex[:10]}"
    create = docker_command(
        host,
        "create",
        "--pull",
        "never",
        "--name",
        name,
        "--network",
        "host",
        "--platform",
        "linux/amd64",
        "--entrypoint",
        "/bin/sleep",
        case.image,
        "300",
    )
    start = docker_command(host, "start", name)
    kill = docker_command(host, "kill", name)
    execute = docker_command(host, "exec", name, case.entrypoint, *case.arguments)
    remove = docker_command(host, "rm", name)
    create_ms = elapsed_ms(create)

    def start_once() -> float:
        measured = elapsed_ms(start)
        run_checked(kill)
        return measured

    try:
        start_result = benchmark_operation(
            start_once,
            warmups=warmups,
            runs=runs,
            settle_seconds=settle_seconds,
        )
        run_checked(start)
        exec_result = benchmark_command(
            execute,
            warmups=warmups,
            runs=runs,
            settle_seconds=settle_seconds,
        )
        run_checked(kill)
        remove_ms = elapsed_ms(remove)
    except Exception:
        subprocess.run(
            docker_command(host, "rm", "--force", name),
            check=False,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=120,
        )
        raise
    return start_result, exec_result, create_ms, remove_ms


def benchmark_running_exec(
    host: str,
    case: RuntimeCase,
    *,
    warmups: int,
    runs: int,
    settle_seconds: float,
) -> tuple[dict[str, Any], float, float, float]:
    name = f"rish-bench-exec-{case.name}-{uuid.uuid4().hex[:10]}"
    create = docker_command(
        host,
        "create",
        "--pull",
        "never",
        "--name",
        name,
        "--network",
        "host",
        "--platform",
        "linux/amd64",
        "--entrypoint",
        "/bin/sleep",
        case.image,
        "300",
    )
    start = docker_command(host, "start", name)
    execute = docker_command(host, "exec", name, case.entrypoint, *case.arguments)
    kill = docker_command(host, "kill", name)
    remove = docker_command(host, "rm", name)
    create_ms = elapsed_ms(create)
    try:
        start_ms = elapsed_ms(start)
        result = benchmark_command(
            execute,
            warmups=warmups,
            runs=runs,
            settle_seconds=settle_seconds,
        )
        run_checked(kill)
        remove_ms = elapsed_ms(remove)
    except Exception:
        subprocess.run(
            docker_command(host, "rm", "--force", name),
            check=False,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=120,
        )
        raise
    return result, create_ms, start_ms, remove_ms


def inspect_image(host: str, image: str) -> dict[str, Any]:
    raw = run_checked(
        docker_command(host, "image", "inspect", image, "--format", "{{json .}}"),
        capture=True,
    )
    metadata = json.loads(raw)
    digests = metadata.get("RepoDigests") or []
    return {
        "reference": image,
        "digest": digests[0] if digests else None,
        "id": metadata.get("Id"),
        "size_bytes": metadata.get("Size"),
    }


def print_summary(results: list[dict[str, Any]]) -> None:
    print()
    extended = all("prepared_runtime_start" in result for result in results)
    warm_exec = all("warm_container_exec" in result for result in results)
    header = "runtime  lifecycle median/p95  runtime median/p95"
    if extended:
        header += "  prepared median/p95  start median/p95"
    if warm_exec:
        header += "  warm-exec median/p95"
    print(header)
    for result in results:
        lifecycle = result["lifecycle_run"]
        runtime = result["runtime_run"]
        line = (
            f"{result['runtime']:<8}"
            f" {lifecycle['median_ms']:>8.1f}/{lifecycle['p95_ms']:<8.1f}"
            f" {runtime['median_ms']:>8.1f}/{runtime['p95_ms']:<8.1f}"
        )
        if extended:
            prepared = result["prepared_runtime_start"]
            start = result["prepared_start_to_running"]
            line += (
                f" {prepared['median_ms']:>8.1f}/{prepared['p95_ms']:<8.1f}"
                f" {start['median_ms']:>8.1f}/{start['p95_ms']:<8.1f}"
            )
        if warm_exec:
            execute = result["warm_container_exec"]
            line += f" {execute['median_ms']:>8.1f}/{execute['p95_ms']:<8.1f}"
        print(line)


def main() -> int:
    args = parse_args()
    settle_seconds = args.settle_ms / 1000
    try:
        server = run_checked(
            docker_command(
                args.docker_host,
                "version",
                "--format",
                "{{.Server.Version}} {{.Server.Os}}/{{.Server.Arch}}",
            ),
            capture=True,
        )
        print(
            f"server={server} runs={args.runs} warmups={args.warmups} "
            f"settle_ms={args.settle_ms:g}",
            flush=True,
        )
        results = []
        selected = [
            case for case in CASES if not args.runtime or case.name in args.runtime
        ]
        for case in selected:
            print(f"benchmarking {case.name} ({case.image})", flush=True)
            image = inspect_image(args.docker_host, case.image)
            lifecycle = benchmark_command(
                run_command(args.docker_host, case, lifecycle_only=True),
                warmups=args.warmups,
                runs=args.runs,
                settle_seconds=settle_seconds,
            )
            runtime = benchmark_command(
                run_command(args.docker_host, case, lifecycle_only=False),
                warmups=args.warmups,
                runs=args.runs,
                settle_seconds=settle_seconds,
            )
            result = {
                "runtime": case.name,
                "image": image,
                "lifecycle_run": lifecycle,
                "runtime_run": runtime,
            }
            if args.extended:
                prepared, create_ms, remove_ms = benchmark_prepared_start(
                    args.docker_host,
                    case,
                    warmups=args.warmups,
                    runs=args.runs,
                    settle_seconds=settle_seconds,
                )
                start, execute, keepalive_create_ms, keepalive_remove_ms = (
                    benchmark_keepalive(
                        args.docker_host,
                        case,
                        warmups=args.warmups,
                        runs=args.runs,
                        settle_seconds=settle_seconds,
                    )
                )
                result.update(
                    {
                        "prepared_runtime_start": prepared,
                        "prepared_create_ms": round(create_ms, 3),
                        "prepared_remove_ms": round(remove_ms, 3),
                        "prepared_start_to_running": start,
                        "warm_container_exec": execute,
                        "keepalive_create_ms": round(keepalive_create_ms, 3),
                        "keepalive_remove_ms": round(keepalive_remove_ms, 3),
                    }
                )
            elif args.warm_exec:
                execute, live_create_ms, live_start_ms, live_remove_ms = (
                    benchmark_running_exec(
                        args.docker_host,
                        case,
                        warmups=args.warmups,
                        runs=args.runs,
                        settle_seconds=settle_seconds,
                    )
                )
                result.update(
                    {
                        "warm_container_exec": execute,
                        "keepalive_create_ms": round(live_create_ms, 3),
                        "keepalive_start_ms": round(live_start_ms, 3),
                        "keepalive_remove_ms": round(live_remove_ms, 3),
                    }
                )
            results.append(result)
            progress = (
                f"  lifecycle={lifecycle['median_ms']:.1f} ms "
                f"runtime={runtime['median_ms']:.1f} ms"
            )
            if args.extended:
                progress += (
                    f" prepared={prepared['median_ms']:.1f} ms"
                    f" start={start['median_ms']:.1f} ms"
                    f" warm-exec={execute['median_ms']:.1f} ms"
                )
            elif args.warm_exec:
                progress += f" warm-exec={execute['median_ms']:.1f} ms"
            print(progress, flush=True)
    except (BenchmarkError, FileNotFoundError, subprocess.TimeoutExpired) as error:
        print(f"benchmark failed: {error}", file=sys.stderr)
        return 1

    report = {
        "schema": "rish-language-startup-benchmark-v2",
        "measured_at": datetime.now(timezone.utc).isoformat(),
        "docker_host": args.docker_host,
        "server": server,
        "runs": args.runs,
        "warmups": args.warmups,
        "settle_ms": args.settle_ms,
        "extended": args.extended,
        "warm_exec": args.warm_exec or args.extended,
        "results": results,
    }
    print_summary(results)
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        print(f"wrote {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
