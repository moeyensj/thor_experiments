# Hotspot2D against DBSCAN on the 24 interior regions' window dumps, made at THOR 939d286e (Section 7; MEASURED: timings).
set -euo pipefail
D=/home/moeyensj/projects/thor/thor-rust-experiments/search/step1/sizing_contract_2026-11-03/chance_validation
"$BIN/clustering_speed" --dumps "$D/dump_939d286e" --threads 16 --reps 3 \
  --out-json results/clustering_speed.json --out-tex "$PAPER/generated/clustering_speed.tex"
