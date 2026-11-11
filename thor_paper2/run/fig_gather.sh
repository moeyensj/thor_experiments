# Figure 1: one test orbit's gather (ASSIST), compared with the r07dp2 run's own.
set -euo pipefail
E=/home/moeyensj/projects/thor/thor-rust-experiments
G=$E/search/step1/sizing_contract_2026-11-03/gate1/run_side
T=f7445n32_r2.2249_b00n00_rdot00_e01of03
"$BIN/fig_gather" --catalog "$E/runs/step1/initial-run-r03dp2/local/catalog_20250630_20250730/catalog.json" \
  --dates 2025-06-30,2025-07-10 --config "$G/configs/75104c17511f.json" \
  --contract "$E/runs/step1/tiling-r09-july-full-solar-system-ladder/inner/f7445.ladder.e01of03.footprint4.contract.json" \
  --override-reason "DP2-only campaign (Joachim, 2026-09-18): the July tiling's footprint and seed pixels were built from DP2 alone, but its contracts record no dataset selection, so the run selects lsst_dp2 / X05 explicitly" \
  --test-orbits "$E/runs/step1/initial-run-r02/frames/$T.parquet" --include-datasets lsst_dp2 \
  --labels "$E/runs/step1/full-run-r01dp2/campaign/labels/labels.parquet" --run-ids "$G/B1000/$T.ids" \
  --run-bundle "$E/runs/step1/initial-run-r07dp2/results/$T/bundles/7445/$T/transformed_detections.parquet" \
  --threads 16 --out-dir results/fig_gather --out-tex "$PAPER/generated/fig_gather.tex"
