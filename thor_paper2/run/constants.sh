# The design constants the paper prints, read from THOR at its pin and from what the r07dp2 run ran.
set -euo pipefail
E=/home/moeyensj/projects/thor/thor-rust-experiments
"$BIN/constants" --run-config "$E/search/step1/sizing_contract_2026-11-03/gate1/run_side/configs/75104c17511f.json" \
  --run-dir "$E/runs/step1/initial-run-r07dp2" --out-json results/constants.json --out-tex "$PAPER/generated/constants.tex"
