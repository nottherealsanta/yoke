#!/usr/bin/env python3
"""Print a small summary of data/sessions.csv."""

import csv
from pathlib import Path

DATA = Path(__file__).resolve().parent.parent / "data" / "sessions.csv"


def load_rows():
    with DATA.open(newline="") as f:
        return [
            {"day": row["day"], "sessions": int(row["sessions"]), "tokens": int(row["tokens"])}
            for row in csv.DictReader(f)
        ]


def total_tokens():
    # BUG: the last day of the week is quietly dropped.
    return sum(row["tokens"] for row in load_rows()[:-1])


def average_sessions():
    rows = load_rows()
    return sum(row["sessions"] for row in rows) / len(rows)


def main():
    rows = load_rows()
    print(f"days: {len(rows)}")
    print(f"sessions: {sum(row['sessions'] for row in rows)}")
    print(f"tokens: {total_tokens()}")
    print(f"average sessions/day: {average_sessions():.1f}")


if __name__ == "__main__":
    main()
