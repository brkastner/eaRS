#!/usr/bin/env python3
"""Summarize eaRS review-history.jsonl decisions."""

from __future__ import annotations

import argparse
import json
import os
from collections import Counter
from datetime import datetime
from pathlib import Path
from typing import Any, Iterable


def default_history_path() -> Path:
    state_dir = os.getenv("XDG_STATE_HOME")
    if state_dir:
        return Path(state_dir) / "ears" / "review-history.jsonl"
    home = os.getenv("HOME")
    if home:
        return Path(home) / ".local" / "state" / "ears" / "review-history.jsonl"
    return Path("review-history.jsonl")


def load_records(path: Path) -> list[dict[str, Any]]:
    records: list[dict[str, Any]] = []
    if not path.exists():
        return records
    with path.open("r", encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            try:
                record = json.loads(line)
            except json.JSONDecodeError:
                continue
            if isinstance(record, dict):
                records.append(record)
    return records


def normalize_choice(value: str | None) -> str:
    if not value:
        return "UNKNOWN"
    return value.strip().upper()


def normalize_profile(value: str | None) -> str:
    if not value:
        return "unknown"
    return value.strip().lower()


def selection_safe(record: dict[str, Any]) -> str:
    choice = normalize_choice(record.get("choice"))
    if choice == "RAW":
        return "true"
    if choice == "FINAL":
        return "true" if record.get("llm_safe") else "false"
    if choice == "ACCURACY":
        return "true" if record.get("accuracy_safe") else "false"
    if choice == "CANCEL":
        return "unknown"
    return "unknown"


def has_candidate(record: dict[str, Any], field: str) -> bool:
    value = record.get(field)
    return isinstance(value, str) and value.strip() != ""


def format_ts(ts_ms: int | None) -> str:
    if not ts_ms:
        return "-"
    return datetime.fromtimestamp(ts_ms / 1000.0).strftime("%Y-%m-%d %H:%M:%S")


def summarize(records: Iterable[dict[str, Any]]) -> dict[str, Any]:
    records = list(records)
    choice_counts = Counter()
    profile_counts = Counter()
    accuracy_counts = Counter()
    model_counts = Counter()
    safe_counts = Counter()
    candidate_counts = Counter()
    pick_counts = Counter()

    for record in records:
        choice = normalize_choice(record.get("choice"))
        choice_counts[choice] += 1

        profile_counts[normalize_profile(record.get("profile"))] += 1

        accuracy_counts["on" if record.get("accuracy_enabled") else "off"] += 1

        model = record.get("model") or "(unknown)"
        model_counts[model] += 1

        safe_counts[selection_safe(record)] += 1

        has_llm = has_candidate(record, "llm")
        has_accuracy = has_candidate(record, "accuracy")
        if has_llm:
            candidate_counts["llm"] += 1
        if has_accuracy:
            candidate_counts["accuracy"] += 1
        if has_llm and has_accuracy:
            candidate_counts["both"] += 1

        pick_counts[choice] += 1

    return {
        "total": len(records),
        "choices": dict(choice_counts.most_common()),
        "profiles": dict(profile_counts.most_common()),
        "accuracy": dict(accuracy_counts.most_common()),
        "models": dict(model_counts.most_common(10)),
        "selection_safe": dict(safe_counts.most_common()),
        "candidates": dict(candidate_counts.most_common()),
        "picks": dict(pick_counts.most_common()),
    }


def print_summary(summary: dict[str, Any]) -> None:
    print(f"Total decisions: {summary['total']}")
    print("Choices:")
    for key, value in summary["choices"].items():
        print(f"  {key}: {value}")
    print("Profiles:")
    for key, value in summary["profiles"].items():
        print(f"  {key}: {value}")
    print("Accuracy enabled:")
    for key, value in summary["accuracy"].items():
        print(f"  {key}: {value}")
    print("Selection safety:")
    for key, value in summary["selection_safe"].items():
        print(f"  {key}: {value}")
    print("Candidate availability:")
    for key, value in summary["candidates"].items():
        print(f"  {key}: {value}")
    print("Top models:")
    for key, value in summary["models"].items():
        print(f"  {key}: {value}")


def main() -> int:
    parser = argparse.ArgumentParser(description="Summarize eaRS review history")
    parser.add_argument("--path", type=Path, default=default_history_path())
    parser.add_argument("--limit", type=int, default=0, help="Limit to last N records")
    parser.add_argument("--profile", type=str, default="")
    parser.add_argument("--choice", type=str, default="")
    parser.add_argument("--format", choices=["text", "json"], default="text")
    parser.add_argument("--recent", type=int, default=0, help="Show last N decisions")
    args = parser.parse_args()

    records = load_records(args.path)
    if args.profile:
        profile_filter = args.profile.strip().lower()
        records = [
            r for r in records if normalize_profile(r.get("profile")) == profile_filter
        ]
    if args.choice:
        choice_filter = args.choice.strip().upper()
        records = [
            r for r in records if normalize_choice(r.get("choice")) == choice_filter
        ]

    records.sort(key=lambda r: r.get("ts_ms", 0))
    if args.limit > 0:
        records = records[-args.limit :]

    summary = summarize(records)
    if args.format == "json":
        print(json.dumps(summary, indent=2))
    else:
        print_summary(summary)

    if args.recent > 0:
        print("Recent decisions:")
        for record in records[-args.recent :]:
            ts = format_ts(record.get("ts_ms"))
            choice = normalize_choice(record.get("choice"))
            profile = normalize_profile(record.get("profile"))
            model = record.get("model") or "(unknown)"
            print(f"  {ts} | {profile} | {choice} | {model}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
