#!/bin/bash
# Local demo only. The bridge always passes the full hosted-demo flags
# (--tools/--root/--no-memory/...), which this checkout may not know yet.
# This wrapper ignores them and runs the real TUI with the offline mock
# provider instead. Use the real binary once the CLI contract lands.
set -e
here=$(cd "$(dirname "$0")" && pwd)
exec "${YOKE_REAL:-$here/../target/debug/yoke}" --provider mock --yes
