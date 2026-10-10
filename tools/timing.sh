#!/bin/bash
# Decodes a fixed set of difficult pictures one after the other and prints the seconds each took and
# how many blocks were restored, to compare builds of the decoder.
#
#   tools/timing.sh label path/to/paperback-rs [picture ...]
#
# The set used for the profile in docs/decisions.md is the default. Results go to
# $OUT (default: ./timing) as <label>.txt. Every decode is bounded by a timeout.
label=${1:?label}
BIN=${2:?binary}
shift 2
OUT=${OUT:-timing}
mkdir -p "$OUT"
if [ $# -eq 0 ]; then
  set -- ${PICTURES:?give pictures or set PICTURES}
fi
: > "$OUT/$label.txt"
for f in "$@"; do
  start=$(date +%s%N)
  said=$(timeout "${LIMIT:-1500}" "$BIN" decode -v "$f" -o /dev/null 2>&1)
  secs=$(awk -v a="$start" -v b="$(date +%s%N)" 'BEGIN { printf "%.1f", (b - a) / 1e9 }')
  recovered=$(echo "$said" | grep -oE "[0-9]+ of [0-9]+ blocks recovered" | sed 's/ blocks recovered//')
  [ -z "$recovered" ] && recovered="complete"
  read_line=$(echo "$said" | grep -E "^page 1: [0-9]+ blocks read" | sed -E 's/page 1: ([0-9]+) blocks read, ([0-9]+) unreadable.*/\1 read, \2 unreadable/')
  printf "%-26s %7.1fs  %-22s %s\n" "$(basename "$f")" "$secs" "$recovered" "$read_line" | tee -a "$OUT/$label.txt"
done
awk '{ s += $2 } END { printf "total %.1fs\n", s }' "$OUT/$label.txt" | tee -a "$OUT/$label.txt"
