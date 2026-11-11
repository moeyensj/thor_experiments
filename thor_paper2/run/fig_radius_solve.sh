# Figure 2: E(R) against R on the same dumps, matched to the tiler's record.
set -euo pipefail
D=/home/moeyensj/projects/thor/thor-rust-experiments/search/step1/sizing_contract_2026-11-03/chance_validation
"$BIN/fig_radius_solve" --dumps "$D/dump_939d286e" --tiler-record "$D/run_939d286e/f7962.json" \
  --out-json results/fig_radius_solve.json --out-tex "$PAPER/generated/fig_radius_solve.tex"
