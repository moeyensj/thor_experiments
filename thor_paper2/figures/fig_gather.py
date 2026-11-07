"""THOR 2's gather for one test orbit: the THOR 2 counterpart of THOR 1's Figures 1 and 2 (Moeyens et al. 2021).

Reads what src/bin/fig_gather.rs wrote (results/fig_gather/: detections.parquet and fig_gather.json)
and draws three panels into the paper's generated/fig_gather.pdf, with a PNG preview in figures/preview/ (not committed):

  (a) one exposure on the sky: its detections inside twice the aperture, gathered (blue) or not (grey); the guard of the
      segment that covers the exposure (the aperture the gather tested, black); the covariance ellipse at that instant
      (dashed); the test orbit's predicted position (cross);
  (b) the whole window on the sky: every gathered detection coloured by time; the guards of the track's segments (black;
      dashed where a segment holds no exposure); the covariance ellipse at the first exposure, the exposure nearest the
      test orbit's epoch and the last exposure (thin, coloured by time); the predicted track;
  (c) the gathered detections in the test orbit's frame (theta_x, theta_y from THOR's transform), every known object's
      detections joined by a thin line in time order, and HIGHLIGHT known objects drawn strongly (the rule is in
      `highlighted`); the test orbit sits at the origin.

Every ellipse is drawn from the guard's own fields (centre direction, tangent basis e1/e2, semi-axes, angle) by inverting
THOR's containment test (`OrientedGuard::contains_dir`: angles atan2(d.e1, d.c) and atan2(d.e2, d.c) inside the ellipse), so
the outline is the boundary the gather applied. Run from thor_paper2/ with the paper's venv, PAPER a checkout of
moeyensj/thor_paper2:

    python figures/fig_gather.py $PAPER
"""
from __future__ import annotations

import json
import os
import pathlib
import subprocess
import sys

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt  # noqa: E402
import numpy as np  # noqa: E402
import pyarrow.parquet as pq  # noqa: E402
from matplotlib.colors import ListedColormap  # noqa: E402

import thorstyle as ts  # noqa: E402

ROOT = pathlib.Path(__file__).resolve().parents[1]
RESULTS = ROOT / "results" / "fig_gather"
OUT_PREVIEW = ROOT / "figures" / "preview" / "fig_gather"


def outline(g: dict, n: int = 721) -> tuple[np.ndarray, np.ndarray]:
    """RA, Dec (deg) of a guard's boundary: the points where THOR's containment test equals one."""
    c, e1, e2 = (np.asarray(g[k]) for k in ("center_dir", "e1", "e2"))
    a, b, th = np.radians(g["semi_major_deg"]), np.radians(g["semi_minor_deg"]), np.radians(g["theta_deg"])
    phi = np.linspace(0.0, 2.0 * np.pi, n)
    ap, bp = a * np.cos(phi), b * np.sin(phi)              # principal axes
    al, be = ap * np.cos(th) - bp * np.sin(th), ap * np.sin(th) + bp * np.cos(th)  # angles along e1, e2
    v = c[None, :] + np.tan(al)[:, None] * e1[None, :] + np.tan(be)[:, None] * e2[None, :]
    v /= np.linalg.norm(v, axis=1)[:, None]
    return np.degrees(np.arctan2(v[:, 1], v[:, 0])) % 360.0, np.degrees(np.arcsin(v[:, 2]))


HIGHLIGHT = 6           # known objects drawn strongly in panel (c)
HIGHLIGHT_NIGHTS = 6    # ... each seen on at least this many nights
HIGHLIGHT_SEPARATION = 0.6  # deg: ... and its track's centre this far from every other highlighted one


def highlighted(ids, x, y, nights) -> list:
    """The known objects panel (c) draws strongly: those seen on at least HIGHLIGHT_NIGHTS nights, most detections first
    (ties by id), each kept only if its track's centre lies HIGHLIGHT_SEPARATION from every one already kept."""
    stats = {}
    for i, o in enumerate(ids):
        stats.setdefault(o, []).append(i)
    ranked = sorted((o for o, rows in stats.items() if len(set(nights[rows])) >= HIGHLIGHT_NIGHTS),
                    key=lambda o: (-len(stats[o]), o))
    kept, centres = [], []
    for o in ranked:
        c = np.array([x[stats[o]].mean(), y[stats[o]].mean()])
        if all(np.hypot(*(c - d)) >= HIGHLIGHT_SEPARATION for d in centres):
            kept.append(o)
            centres.append(c)
        if len(kept) == HIGHLIGHT:
            break
    return kept


def sky_axes(ax, dec0: float) -> None:
    """RA increasing to the left, and equal angles on the sky at the panel's declination."""
    ax.invert_xaxis()
    ax.set_aspect(1.0 / np.cos(np.radians(dec0)), adjustable="datalim")
    ax.set_xlabel(r"$\alpha$ (deg)")
    ax.set_ylabel(r"$\delta$ (deg)")


def paper_dir() -> pathlib.Path:
    """The checkout of moeyensj/thor_paper2 the figure is drawn into: the script's one argument."""
    if len(sys.argv) != 2 or not (pathlib.Path(sys.argv[1]) / "generated").is_dir():
        raise SystemExit(f"usage: python {sys.argv[0]} PAPER   (PAPER: a checkout of moeyensj/thor_paper2)")
    return pathlib.Path(sys.argv[1])


def main() -> None:
    out_pdf = paper_dir() / "generated" / "fig_gather"
    os.environ.setdefault("SOURCE_DATE_EPOCH", "0")  # a PDF that does not change when nothing it shows changed
    rec = json.loads((RESULTS / "fig_gather.json").read_text())
    tab = pq.read_table(RESULTS / "detections.parquet")
    col = {k: tab.column(k).to_numpy(zero_copy_only=False) for k in tab.column_names}
    gathered = col["gathered"].astype(bool)
    t0 = rec["test_orbit"]["epoch_tdb"]
    segments = {s["seg_id"]: s for s in rec["segments"]}
    exposures = rec["exposures"]

    ts.use()
    fig, (axa, axb, axc) = plt.subplots(1, 3, figsize=ts.figsize("page", 0.36))
    blue, crimson = ts.SLOTS["ink blue"], ts.SLOTS["crimson"]
    grey = ts.NEUTRAL_CONTEXT
    # Time on a light-to-dark blue ramp, its lightest third cut so the earliest detections stay visible on the page.
    cmap = ListedColormap(ts.SEQUENTIAL(np.linspace(0.3, 1.0, 256)), name="thor_blue_cut")
    t_det = col["time_utc"]
    tmin, tmax = float(np.min(t_det[gathered])) - t0, float(np.max(t_det[gathered])) - t0

    # ── (a) one exposure ──────────────────────────────────────────────────────────────────────────────────────────
    pa = rec["panel_a_exposure"]
    exp = exposures[pa["index"]]
    sel = col["state_id"] == np.uint64(int(pa["state_id"]))
    out = sel & ~gathered
    inn = sel & gathered
    axa.scatter(col["ra"][out], col["dec"][out], s=0.6, c=grey, lw=0, rasterized=True)
    axa.scatter(col["ra"][inn], col["dec"][inn], s=0.6, c=blue, lw=0, rasterized=True)
    for k in exp["segments"]:
        axa.plot(*outline(segments[k]["guard"]), color=ts.INK, lw=0.8)
    axa.plot(*outline(exp["epoch_guard"]), color=ts.INK, lw=0.6, ls=(0, (4, 2)))
    axa.plot(exp["predicted_ra_deg"], exp["predicted_dec_deg"], marker="+", color=crimson, ms=6, mew=1.0, ls="none")
    sky_axes(axa, exp["predicted_dec_deg"])
    ts.tidy(axa, 4, 4)

    # ── (b) the window ────────────────────────────────────────────────────────────────────────────────────────────
    order = np.argsort(t_det[gathered])
    sc = axb.scatter(col["ra"][gathered][order], col["dec"][gathered][order], s=0.05, c=t_det[gathered][order] - t0,
                     cmap=cmap, vmin=tmin, vmax=tmax, lw=0, rasterized=True)
    nearest = min(exposures, key=lambda e: abs(e["epoch_tdb"] - t0))
    for e in (exposures[0], nearest, exposures[-1]):
        axb.plot(*outline(e["epoch_guard"]), color=cmap((e["time_utc"] - t0 - tmin) / (tmax - tmin)), lw=0.6)
    for s in rec["segments"]:
        holds = any(s["seg_id"] in e["segments"] for e in exposures)
        axb.plot(*outline(s["guard"]), color=ts.INK, lw=0.8, ls="-" if holds else (0, (4, 2)))
    axb.plot([e["predicted_ra_deg"] for e in exposures], [e["predicted_dec_deg"] for e in exposures], color=crimson,
             lw=1.0, solid_capstyle="round")
    sky_axes(axb, float(np.median([e["predicted_dec_deg"] for e in exposures])))
    ts.tidy(axb, 4, 4)
    cax = axb.inset_axes([0.06, 0.115, 0.34, 0.03])
    cb = fig.colorbar(sc, cax=cax, orientation="horizontal", ticks=[-4, -2, 0, 2])
    cb.set_label(r"$t - t_0$ (d)", labelpad=1.5)
    cax.xaxis.set_label_position("top")
    cb.outline.set_linewidth(0.5)
    cax.tick_params(width=0.5, length=2, pad=1.5, direction="out", which="both", top=False)
    cax.minorticks_off()

    # ── (c) the test orbit's frame ────────────────────────────────────────────────────────────────────────────────
    tx, ty = col["theta_x"].astype(float), col["theta_y"].astype(float)
    known = gathered & np.array([o is not None for o in col["ss_object_id"]])
    axc.scatter(tx[gathered & ~known], ty[gathered & ~known], s=0.05, c=grey, lw=0, rasterized=True)
    ids = col["ss_object_id"][known]
    kx, ky, kt, kn = tx[known], ty[known], t_det[known], col["night"][known]
    o = np.lexsort((kt, ids))
    ids, kx, ky, kn = ids[o], kx[o], ky[o], kn[o]
    breaks = np.flatnonzero(ids[1:] != ids[:-1]) + 1
    tracks = {i[0]: np.column_stack((x, y)) for i, x, y in zip(np.split(ids, breaks), np.split(kx, breaks),
                                                                np.split(ky, breaks))}
    from matplotlib.collections import LineCollection
    axc.add_collection(LineCollection([v for v in tracks.values() if len(v) > 1], colors=ts.SLOTS["periwinkle"],
                                      linewidths=0.25, alpha=0.7, rasterized=True))
    strong = highlighted(ids, kx, ky, kn)
    for oid in strong:
        axc.plot(tracks[oid][:, 0], tracks[oid][:, 1], color=blue, lw=0.8, marker="o", ms=1.6, mew=0)
    des = dict(zip(col["ss_object_id"][known], col["designation"][known]))
    axc.plot(0.0, 0.0, marker="+", color=crimson, ms=6, mew=1.0, ls="none")
    axc.set_aspect("equal", adjustable="datalim")
    axc.set_xlabel(r"$\theta_x$ (deg)")
    axc.set_ylabel(r"$\theta_y$ (deg)")
    ts.tidy(axc, 5, 5)

    for ax, letter in zip((axa, axb, axc), "abc"):
        ts.panel_label(ax, letter)

    provenance = {"figure": "thor_paper2/figures/fig_gather.py", "thor": rec["thor_pin"][:12],
                  "test_orbit": rec["test_orbit"]["id"],
                  "data": "thor_paper2/results/fig_gather/detections.parquet sha256 " + rec["table"]["sha256"][:16],
                  "experiments": subprocess.run(["git", "-C", str(ROOT), "rev-parse", "--short=12", "HEAD"],
                                                capture_output=True, text=True).stdout.strip() or "unknown"}
    ts.savefig(fig, out_pdf, provenance=provenance, formats=("pdf",))
    ts.savefig(fig, OUT_PREVIEW, provenance=provenance, formats=("png",))
    print(f"wrote {out_pdf.with_suffix('.pdf')} and {OUT_PREVIEW.with_suffix('.png')}")
    print("panel (c) highlights:", ", ".join(f"{des[o]} ({len(tracks[o])} detections)" for o in strong))


if __name__ == "__main__":
    main()
