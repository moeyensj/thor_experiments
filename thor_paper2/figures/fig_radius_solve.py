"""THOR 2's radius solve: the expected chance groupings E(R) against the clustering radius R for real windows.

Reads what experiments/src/bin/fig_radius_solve.rs wrote (experiments/results/fig_radius_solve.json) and draws one
column-width panel into generated/fig_radius_solve.pdf, with a PNG preview in figures/preview/ (not committed):

  log E against log R, one line per window (THOR's clustering::background over the window's kept detections, at the
  window's rule, with the hypotheses unbounded as the sizing function prices them), coloured from the sparsest background
  to the densest; the budget B (horizontal); the astrometric floor (vertical); and each window's solved radius, where its
  curve first crosses B.

Run from the repository root with the paper's venv:

    python figures/fig_radius_solve.py
"""
from __future__ import annotations

import hashlib
import json
import os
import pathlib
import subprocess

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt  # noqa: E402
import numpy as np  # noqa: E402
from matplotlib.ticker import FixedLocator, FuncFormatter, LogLocator, NullFormatter  # noqa: E402

import thorstyle as ts  # noqa: E402

ROOT = pathlib.Path(__file__).resolve().parents[1]
RESULTS = ROOT / "experiments" / "results" / "fig_radius_solve.json"
OUT_PDF = ROOT / "generated" / "fig_radius_solve"
OUT_PREVIEW = ROOT / "figures" / "preview" / "fig_radius_solve"

Y_LIMITS = (1e-6, 1e6)  # the curves start near 1e-7 at 0.1" and peak below 1e5


def main() -> None:
    os.environ.setdefault("SOURCE_DATE_EPOCH", "0")  # a PDF that does not change when nothing it shows changed
    raw = RESULTS.read_bytes()
    rec = json.loads(raw)
    radius = np.asarray(rec["radius_arcsec"])
    budget, floor = rec["budget"], rec["astrometric_floor_arcsec"]
    curves = sorted(rec["curves"], key=lambda c: c["window"]["density_per_deg2_per_night"])

    ts.use()
    fig, ax = plt.subplots(figsize=ts.figsize("column", 0.75))
    ax.set_xscale("log")
    ax.set_yscale("log")

    ax.axhline(budget, color=ts.INK, lw=0.8, ls=(0, (5, 2)), zorder=1)
    ax.axvline(floor, color=ts.INK_SECONDARY, lw=0.8, zorder=1)
    for c, colour in zip(curves, ts.CATEGORICAL):
        e = np.asarray(c["e_false"])
        density = c["window"]["density_per_deg2_per_night"]
        ax.plot(radius, e, color=colour, lw=1.0, label=f"{density:,.0f}", zorder=2)
        ax.plot(c["solved_radius_arcsec"], budget, marker="o", ms=3.6, color=colour, mec=ts.PAPER, mew=0.6, ls="none",
                zorder=3)

    ax.set_xlim(radius[0], radius[-1])
    ax.set_ylim(*Y_LIMITS)
    ax.xaxis.set_major_locator(FixedLocator([0.1, 1, 10, 100]))
    ax.xaxis.set_major_formatter(FuncFormatter(lambda v, _: f"{v:g}"))
    ax.yaxis.set_major_locator(LogLocator(base=100.0))
    ax.yaxis.set_minor_locator(LogLocator(base=10.0, subs=(1.0,), numticks=20))
    ax.yaxis.set_minor_formatter(NullFormatter())
    ax.set_xlabel(r"$R$ (arcsec)")
    ax.set_ylabel(r"$E(R)$")

    small = matplotlib.rcParams["legend.fontsize"]
    ax.annotate(rf"$B = {budget:g}$", xy=(radius[0], budget), xytext=(3, 2), textcoords="offset points", ha="left",
                va="bottom", fontsize=small, color=ts.INK)
    ax.annotate("floor", xy=(floor, Y_LIMITS[1]), xytext=(2, -3), textcoords="offset points", ha="left", va="top",
                fontsize=small, color=ts.INK_SECONDARY)
    ax.legend(title=r"Detections deg$^{-2}$ night$^{-1}$", loc="lower right", title_fontsize=small)

    pin = subprocess.run(["git", "-C", str(ROOT), "rev-parse", "--short=12", "HEAD"], capture_output=True, text=True)
    provenance = {"figure": "figures/fig_radius_solve.py", "thor": rec["thor_pin"][:12],
                  "data": "experiments/results/fig_radius_solve.json (" + hashlib.sha256(raw).hexdigest()[:12] + ")",
                  "repo": pin.stdout.strip() or "unknown"}
    ts.savefig(fig, OUT_PDF, provenance=provenance, formats=("pdf",))
    ts.savefig(fig, OUT_PREVIEW, provenance=provenance, formats=("png",))
    print(f"wrote {OUT_PDF.with_suffix('.pdf')} and {OUT_PREVIEW.with_suffix('.png')}")
    for c in curves:
        w = c["window"]
        print(f"{w['region']} w{w['window']}: {w['density_per_deg2_per_night']:,.0f} /deg2/night, "
              f"R = {c['solved_radius_arcsec']:.2f}\", slope {c['slope_dlnE_dlnR_at_solved_radius']:.2f}")


if __name__ == "__main__":
    main()
