//! The radius solve: expected chance groupings E(R) against the clustering radius R, for real windows.
//!
//! Input: the window dumps written by THOR's sizing tiler (`sizing_tile` with `dump_kept_dir`; the same inputs as
//! `clustering_speed`). Each region has a JSON (its windows: radius, rule, velocity envelope Omega_v, what set the radius;
//! the budget B, the astrometric floor and the window length the tiler sized at) and a Parquet of its windows'
//! detections in the test orbit's frame, flagged `kept` where the sweep filter keeps them at the window's radius.
//! Optionally, the tiler's own record of the same sweep (`--tiler-record`, the `f<pixel>.json` holding every region's
//! sizing), which states whether each window's radius settled against the sweep filter.
//!
//! Everything is THOR's own model (`thor::clustering::background`), called as the sizing function calls it
//! (`thor::sizing`, `price_background`): the background of a window is `WindowBackground::from_detections` over its
//! KEPT detections; the rule is the window's; the hypotheses are unbounded (`h = infinity`, so
//! `H_eff = Omega_v W^2 / R^2`, `effective_hypotheses`); W is the rung's window length (the JSON's top-level
//! `window_days`, which the sizing function passes for every window of the rung, the clipped last one included).
//!
//! 1. For every window whose radius the background solve set (`radius_set_by = background_solve`), solve
//!    `E(R) = B` with `solve_radii` and compare the root with the dumped `radius_deg`. A window reproduces when they
//!    agree to `thor::sizing::SWEEP_FILTER_RADIUS_TOL`, the tolerance the tiler settles the radius to.
//! 2. Among the reproduced windows, choose four that span the measured background: those at the minimum, the 1/3 and
//!    2/3 quantiles and the maximum of the kept-detection density (`density_per_deg2_per_night`).
//! 3. For each, evaluate `e_false` on a log grid of R from 0.1" to 200" and the local slope d ln E / d ln R at the
//!    solved radius.
//!
//! Usage:
//!   fig_radius_solve --dumps DIR [--tiler-record PATH] --out-json PATH --out-tex PATH
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use arrow::array::{Array, BooleanArray, Float64Array, Int64Array, UInt32Array, UInt64Array};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use thor::clustering::background::{
    e_false, effective_hypotheses, solve_radii, FalseClusterRule, RadiusSolution, WindowBackground,
};
use thor::sizing::SWEEP_FILTER_RADIUS_TOL;

const ARCSEC: f64 = 1.0 / 3600.0;
/// The curves' log grid of radii (arcsec).
const CURVE_R_MIN_ARCSEC: f64 = 0.1;
const CURVE_R_MAX_ARCSEC: f64 = 200.0;
const CURVE_POINTS: usize = 241;
/// Half step in ln R of the central difference that gives the slope at the solved radius.
const SLOPE_HALF_STEP_LN: f64 = 0.01;
/// The density quantiles of the windows drawn.
const DENSITY_QUANTILES: [f64; 4] = [0.0, 1.0 / 3.0, 2.0 / 3.0, 1.0];

struct Args {
    dumps: PathBuf,
    tiler_record: Option<PathBuf>,
    out_json: PathBuf,
    out_tex: PathBuf,
}

fn parse_args() -> Args {
    let mut a = std::env::args().skip(1);
    let (mut dumps, mut tiler_record, mut out_json, mut out_tex) = (None, None, None, None);
    while let Some(k) = a.next() {
        let v = a.next().unwrap_or_else(|| panic!("{k} needs a value"));
        match k.as_str() {
            "--dumps" => dumps = Some(PathBuf::from(v)),
            "--tiler-record" => tiler_record = Some(PathBuf::from(v)),
            "--out-json" => out_json = Some(PathBuf::from(v)),
            "--out-tex" => out_tex = Some(PathBuf::from(v)),
            _ => panic!("unknown argument {k}"),
        }
    }
    Args {
        dumps: dumps.expect("--dumps"),
        tiler_record,
        out_json: out_json.expect("--out-json"),
        out_tex: out_tex.expect("--out-tex"),
    }
}

fn sha256(path: &Path) -> String {
    let mut f = File::open(path).expect("open input");
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).expect("read input");
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    format!("{:x}", h.finalize())
}

/// Every window's KEPT detections in one dump, as `(state_id, night, theta_x, theta_y)`, the tuple
/// `WindowBackground::from_detections` reads (and `swept_window_background` passes it).
fn read_kept(parquet: &Path) -> HashMap<u32, Vec<(u64, i64, f64, f64)>> {
    let file = File::open(parquet).expect("open parquet");
    let reader = ParquetRecordBatchReaderBuilder::try_new(file).expect("parquet").build().expect("reader");
    let mut out: HashMap<u32, Vec<(u64, i64, f64, f64)>> = HashMap::new();
    for batch in reader {
        let b = batch.expect("batch");
        let col = |name: &str| b.column(b.schema().index_of(name).expect(name)).clone();
        let window = col("window");
        let window = window.as_any().downcast_ref::<UInt32Array>().expect("window u32");
        let kept = col("kept");
        let kept = kept.as_any().downcast_ref::<BooleanArray>().expect("kept bool");
        let state = col("state_id");
        let state = state.as_any().downcast_ref::<UInt64Array>().expect("state_id");
        let night = col("night");
        let night = night.as_any().downcast_ref::<Int64Array>().expect("night");
        let tx = col("theta_x");
        let tx = tx.as_any().downcast_ref::<Float64Array>().expect("theta_x");
        let ty = col("theta_y");
        let ty = ty.as_any().downcast_ref::<Float64Array>().expect("theta_y");
        for i in 0..b.num_rows() {
            if kept.value(i) {
                out.entry(window.value(i)).or_default().push((state.value(i), night.value(i), tx.value(i), ty.value(i)));
            }
        }
    }
    out
}

/// The tiler record's windows, keyed by the dump's region (its file stem without the share, [`region_key`]) and the
/// window index: (settled, rounds, radius_deg, r2). The key is built with the tiler's own format (`sizing::tile::dump_tag`).
fn read_tiler_record(path: &Path) -> HashMap<(String, u64), (Option<bool>, Option<u64>, f64, Value)> {
    let rec: Value = serde_json::from_reader(File::open(path).expect("tiler record")).expect("parse tiler record");
    let budget = rec["budget"].as_f64().expect("record budget");
    let mut out = HashMap::new();
    for t in rec["tried"].as_array().expect("tried") {
        if t["outcome"]["outcome"].as_str() != Some("accepted") {
            continue;
        }
        let r = &t["region"];
        let f = |k: &str| r[k].as_f64().unwrap_or_else(|| panic!("region {k}"));
        let stem = format!(
            "p{}_n{}_d{}_r{:.5}_v{:+.5}_a{:+.6}_o{:+.6}_B{}",
            r["pixel"].as_u64().expect("pixel"),
            r["nside"].as_u64().expect("nside"),
            r["depth"].as_u64().expect("depth"),
            f("rho_lo"),
            f("rdot_lo"),
            f("along_lo"),
            f("normal_lo"),
            budget
        );
        for w in t["outcome"]["windows"].as_array().expect("windows") {
            let sf = &w["sweep_filter"];
            out.insert(
                (stem.clone(), w["index"].as_u64().expect("index")),
                (sf["settled"].as_bool(), sf["rounds"].as_u64(), w["radius_deg"].as_f64().expect("radius"), w["r2"].clone()),
            );
        }
    }
    out
}

/// A dump's file stem without its share. The tiler names a dump `<region>_S<start>+<days>_B<budget>` since D-024
/// (`sizing::tile::dump_tag`), and `<region>_B<budget>` before; the record's regions carry no share, so both forms key
/// by `<region>_B<budget>`.
fn region_key(stem: &str) -> String {
    match (stem.find("_S"), stem.rfind("_B")) {
        (Some(i), Some(j)) if i < j => format!("{}{}", &stem[..i], &stem[j..]),
        _ => stem.to_string(),
    }
}

fn solution_json(s: RadiusSolution) -> Value {
    match s {
        RadiusSolution::Solved(r) => json!({"solved_arcsec": r / ARCSEC}),
        RadiusSolution::BelowGrid(r) => json!({"below_grid_arcsec": r / ARCSEC}),
        RadiusSolution::AboveGrid(r) => json!({"above_grid_arcsec": r / ARCSEC}),
        RadiusSolution::TooFewNights => json!("too_few_nights"),
    }
}

/// One window whose radius the background solve set, re-solved on its kept detections.
struct Solved {
    region: String,
    index: u64,
    bg: WindowBackground,
    rule: FalseClusterRule,
    omega_v: f64,
    w_days: f64,
    w_window_days: f64,
    radius_dump_deg: f64,
    solution: RadiusSolution,
    rel_mismatch: f64,
    e_at_dump_radius: f64,
    tiler: Option<(Option<bool>, Option<u64>, f64, Value)>,
}

impl Solved {
    fn reproduced(&self) -> bool {
        self.rel_mismatch.is_finite() && self.rel_mismatch <= SWEEP_FILTER_RADIUS_TOL
    }

    fn row(&self) -> Value {
        let (settled, rounds, record_radius, r2) = match &self.tiler {
            Some((s, n, r, r2)) => (json!(s), json!(n), json!(r / ARCSEC), r2.clone()),
            None => (Value::Null, Value::Null, Value::Null, Value::Null),
        };
        json!({
            "region": self.region, "window": self.index,
            "rule": {"min_obs": self.rule.min_obs, "min_nights": self.rule.min_nights},
            "n_kept": self.bg.n_detections, "n_visits": self.bg.n_visits, "n_nights": self.bg.n_nights,
            "occupied_area_deg2": self.bg.occupied_area_deg2,
            "density_per_deg2_per_night": self.bg.density_per_deg2_per_night(),
            "omega_v_deg2_per_day2": self.omega_v, "window_days": self.w_days, "window_t1_minus_t0_days": self.w_window_days,
            "radius_dump_arcsec": self.radius_dump_deg / ARCSEC,
            "solve": solution_json(self.solution),
            "relative_mismatch": if self.rel_mismatch.is_finite() { json!(self.rel_mismatch) } else { Value::Null },
            "reproduced": self.reproduced(),
            "e_false_at_dump_radius": self.e_at_dump_radius,
            "tiler_record": {"settled": settled, "rounds": rounds, "radius_arcsec": record_radius, "r2_deg": r2},
        })
    }
}

fn quantile_index(n: usize, q: f64) -> usize {
    ((q * (n - 1) as f64).round() as usize).min(n - 1)
}

fn main() {
    let args = parse_args();
    let tiler = args.tiler_record.as_deref().map(read_tiler_record);

    let mut jsons: Vec<PathBuf> = fs::read_dir(&args.dumps)
        .expect("dumps dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    jsons.sort();

    let mut inputs = Vec::new();
    let (mut budget, mut floor_deg): (Option<f64>, Option<f64>) = (None, None);
    let mut set_by_counts: HashMap<String, usize> = HashMap::new();
    let mut solved: Vec<Solved> = Vec::new();
    let mut n_regions = 0usize;
    for json_path in &jsons {
        let parquet = json_path.with_extension("parquet");
        if !parquet.exists() {
            continue;
        }
        n_regions += 1;
        let stem = json_path.file_stem().unwrap().to_string_lossy().to_string();
        let meta: Value = serde_json::from_reader(File::open(json_path).expect("json")).expect("parse json");
        inputs.push(json!({"file": json_path.file_name().unwrap().to_string_lossy(), "sha256": sha256(json_path)}));
        inputs.push(json!({"file": parquet.file_name().unwrap().to_string_lossy(), "sha256": sha256(&parquet)}));
        // One budget and one floor for the whole figure: a dump sized under another is refused, not mixed in.
        let b = meta["budget"].as_f64().expect("budget");
        let fl = meta["astrometric_floor_deg"].as_f64().expect("astrometric_floor_deg");
        assert_eq!(*budget.get_or_insert(b), b, "{stem}: budget {b} differs from the other dumps'");
        assert_eq!(*floor_deg.get_or_insert(fl), fl, "{stem}: astrometric floor {fl} differs from the other dumps'");
        let w_days = meta["window_days"].as_f64().expect("window_days");
        let kept = read_kept(&parquet);
        for w in meta["windows"].as_array().expect("windows") {
            let set_by = w["radius_set_by"].as_str().expect("radius_set_by").to_string();
            *set_by_counts.entry(set_by.clone()).or_default() += 1;
            if set_by != "background_solve" {
                continue;
            }
            let index = w["index"].as_u64().expect("index");
            let rule = FalseClusterRule::new(
                w["rule"]["min_obs"].as_u64().expect("rule") as usize,
                w["rule"]["min_nights"].as_u64().expect("rule") as usize,
            );
            let omega_v = w["omega_v_deg2_per_day2"].as_f64().expect("omega_v");
            let radius_dump_deg = w["radius_deg"].as_f64().expect("radius_deg");
            let dets = kept.get(&(index as u32)).cloned().unwrap_or_default();
            let bg = WindowBackground::from_detections(dets)
                .unwrap_or_else(|| panic!("{stem} w{index}: set by the background solve but holds no kept detection"));
            // Cross-check the H the dump recorded: the sizing function's, h = infinity at the rung's W.
            let h_dump = w["hypotheses"].as_f64().expect("hypotheses");
            let h_here = effective_hypotheses(f64::INFINITY, omega_v, w_days, radius_dump_deg);
            assert!(
                ((h_here - h_dump) / h_dump).abs() < 1e-12,
                "{stem} w{index}: hypotheses {h_dump} recorded, {h_here} from h = inf at W = {w_days}"
            );
            let solution = solve_radii(&bg, &[rule], b, f64::INFINITY, omega_v, w_days)[0];
            let rel_mismatch = match solution {
                RadiusSolution::Solved(r) => (r - radius_dump_deg).abs() / radius_dump_deg,
                _ => f64::INFINITY,
            };
            let e_at_dump_radius = e_false(&bg, rule, f64::INFINITY, omega_v, w_days, radius_dump_deg);
            let s = Solved {
                region: stem.clone(),
                index,
                bg,
                rule,
                omega_v,
                w_days,
                w_window_days: w["t1_utc"].as_f64().expect("t1") - w["t0_utc"].as_f64().expect("t0"),
                radius_dump_deg,
                solution,
                rel_mismatch,
                e_at_dump_radius,
                tiler: tiler.as_ref().map(|t| {
                    t.get(&(region_key(&stem), index))
                        .cloned()
                        .unwrap_or_else(|| panic!("{stem} w{index}: no window in the tiler record"))
                }),
            };
            eprintln!(
                "{stem} w{index}: {} kept, {} nights, {:.0} /deg2/night, R dump {:.4}\", solve {:?}, mismatch {:.2e}, E(R dump) {:.2}, record {:?}",
                s.bg.n_detections,
                s.bg.n_nights,
                s.bg.density_per_deg2_per_night(),
                radius_dump_deg / ARCSEC,
                solution_json(solution),
                rel_mismatch,
                e_at_dump_radius,
                s.tiler.as_ref().map(|t| (t.0, t.1))
            );
            solved.push(s);
        }
    }
    let budget = budget.expect("at least one dump");
    let floor_deg = floor_deg.expect("at least one dump");
    if let Some(t) = &tiler {
        // The tiler record and the dumps must describe the same sizing.
        for s in &solved {
            let (_, _, r, _) = s.tiler.as_ref().unwrap_or_else(|| panic!("{} w{} not in the tiler record", s.region, s.index));
            assert_eq!(*r, s.radius_dump_deg, "{} w{}: the record's radius differs from the dump's", s.region, s.index);
        }
        assert!(!t.is_empty());
    }

    // The windows drawn: the reproduced ones at the density quantiles.
    let mut candidates: Vec<&Solved> = solved.iter().filter(|s| s.reproduced()).collect();
    candidates.sort_by(|a, b| a.bg.density_per_deg2_per_night().total_cmp(&b.bg.density_per_deg2_per_night()));
    assert!(candidates.len() >= DENSITY_QUANTILES.len(), "too few reproduced windows to choose from");
    let chosen: Vec<&Solved> =
        DENSITY_QUANTILES.iter().map(|&q| candidates[quantile_index(candidates.len(), q)]).collect();

    let grid: Vec<f64> = (0..CURVE_POINTS)
        .map(|i| {
            let (lo, hi) = (CURVE_R_MIN_ARCSEC.ln(), CURVE_R_MAX_ARCSEC.ln());
            (lo + (hi - lo) * i as f64 / (CURVE_POINTS - 1) as f64).exp()
        })
        .collect();
    let mut curves = Vec::new();
    let (mut radii, mut slopes, mut densities) = (vec![], vec![], vec![]);
    for (q, s) in DENSITY_QUANTILES.iter().zip(&chosen) {
        let RadiusSolution::Solved(r) = s.solution else { unreachable!("a reproduced window is solved") };
        let e = |rad_deg: f64| e_false(&s.bg, s.rule, f64::INFINITY, s.omega_v, s.w_days, rad_deg);
        let values: Vec<f64> = grid.iter().map(|&ra| e(ra * ARCSEC)).collect();
        let slope = (e(r * SLOPE_HALF_STEP_LN.exp()).ln() - e(r * (-SLOPE_HALF_STEP_LN).exp()).ln())
            / (2.0 * SLOPE_HALF_STEP_LN);
        radii.push(r / ARCSEC);
        slopes.push(slope);
        densities.push(s.bg.density_per_deg2_per_night());
        eprintln!(
            "drawn (density quantile {q:.3}): {} w{}: {:.0} /deg2/night, R = {:.3}\", slope {:.3}, H_eff(R) {:.1}",
            s.region,
            s.index,
            s.bg.density_per_deg2_per_night(),
            r / ARCSEC,
            slope,
            effective_hypotheses(f64::INFINITY, s.omega_v, s.w_days, r)
        );
        curves.push(json!({
            "density_quantile": q,
            "window": s.row(),
            "solved_radius_arcsec": r / ARCSEC,
            "hypotheses_at_solved_radius": effective_hypotheses(f64::INFINITY, s.omega_v, s.w_days, r),
            "slope_dlnE_dlnR_at_solved_radius": slope,
            "e_false_at_floor": e(floor_deg),
            "e_false": values,
        }));
    }

    let minmax = |v: &[f64]| {
        (v.iter().copied().fold(f64::INFINITY, f64::min), v.iter().copied().fold(f64::NEG_INFINITY, f64::max))
    };
    let (r_lo, r_hi) = minmax(&radii);
    let (s_lo, s_hi) = minmax(&slopes);
    let (d_lo, d_hi) = minmax(&densities);
    let n_reproduced = solved.iter().filter(|s| s.reproduced()).count();
    let max_mismatch_reproduced =
        solved.iter().filter(|s| s.reproduced()).map(|s| s.rel_mismatch).fold(0.0f64, f64::max);
    let all_dump_radii: Vec<f64> = solved.iter().map(|s| s.radius_dump_deg / ARCSEC).collect();
    let (all_lo, all_hi) = minmax(&all_dump_radii);
    let mut set_by: Vec<(String, usize)> = set_by_counts.into_iter().collect();
    set_by.sort();

    if let Some(p) = &args.tiler_record {
        inputs.push(json!({"file": p.file_name().unwrap().to_string_lossy(), "sha256": sha256(p), "role": "tiler record"}));
    }
    let summary = json!({
        "regions": n_regions,
        "windows_by_radius_set_by": set_by.iter().map(|(k, v)| (k.clone(), json!(v))).collect::<serde_json::Map<_, _>>(),
        "background_solve_windows": solved.len(),
        "reproduced": n_reproduced,
        "max_relative_mismatch_reproduced": max_mismatch_reproduced,
        "not_reproduced": solved.iter().filter(|s| !s.reproduced()).map(|s| json!({"region": s.region, "window": s.index}))
            .collect::<Vec<_>>(),
        "background_solve_radius_arcsec_min": all_lo, "background_solve_radius_arcsec_max": all_hi,
        "drawn_solved_radius_arcsec_min": r_lo, "drawn_solved_radius_arcsec_max": r_hi,
        "drawn_slope_min": s_lo, "drawn_slope_max": s_hi,
        "drawn_density_min": d_lo, "drawn_density_max": d_hi,
    });
    let record = json!({
        "experiment": "fig_radius_solve",
        "experiments_commit": env!("EXPERIMENTS_COMMIT"), "experiments_source": env!("EXPERIMENTS_SOURCE"),
        "thor_pin": env!("THOR_PIN"), "thor_source": env!("THOR_SOURCE"),
        "model": "thor::clustering::background: WindowBackground::from_detections over the kept detections, e_false and \
                  solve_radii at the window's rule, h = infinity (H_eff = Omega_v W^2 / R^2), W = the rung's window_days",
        "selection": "windows whose radius the background solve set and whose kept-detection solve reproduces the dumped \
                      radius to SWEEP_FILTER_RADIUS_TOL; drawn at the minimum, 1/3, 2/3 and maximum of their kept density",
        "reproduce_tolerance": SWEEP_FILTER_RADIUS_TOL,
        "budget": budget,
        "astrometric_floor_arcsec": floor_deg / ARCSEC,
        "slope_half_step_ln": SLOPE_HALF_STEP_LN,
        "radius_arcsec": grid,
        "inputs": inputs,
        "summary": summary,
        "curves": curves,
        "background_solve_windows": solved.iter().map(Solved::row).collect::<Vec<_>>(),
    });
    if let Some(dir) = args.out_json.parent() {
        fs::create_dir_all(dir).ok();
    }
    fs::write(&args.out_json, serde_json::to_string_pretty(&record).unwrap()).expect("write json");

    // Numbers for the manuscript, as LaTeX macros: never typed by hand.
    let pin = &env!("THOR_PIN")[..8.min(env!("THOR_PIN").len())];
    let exp = env!("EXPERIMENTS_COMMIT_SHORT");
    let tex = format!(
        "%% Generated by thor_experiments/thor_paper2/src/bin/fig_radius_solve.rs (thor_experiments {exp}, THOR {pin}); \
         do not edit.\n\
         \\newcommand{{\\frsRegions}}{{{n_regions}}}\n\
         \\newcommand{{\\frsSolveWindows}}{{{nsolve}}}\n\
         \\newcommand{{\\frsReproduced}}{{{n_reproduced}}}\n\
         \\newcommand{{\\frsWindows}}{{{ndrawn}}}\n\
         \\newcommand{{\\frsBudget}}{{{budget}}}\n\
         \\newcommand{{\\frsFloorArcsec}}{{{floor}}}\n\
         \\newcommand{{\\frsRadiusMin}}{{{r_lo:.1}}}\n\
         \\newcommand{{\\frsRadiusMax}}{{{r_hi:.1}}}\n\
         \\newcommand{{\\frsDensityMin}}{{{d_lo:.0}}}\n\
         \\newcommand{{\\frsDensityMax}}{{{d_hi:.0}}}\n\
         \\newcommand{{\\frsSlopeMin}}{{{s_lo:.1}}}\n\
         \\newcommand{{\\frsSlopeMax}}{{{s_hi:.1}}}\n",
        nsolve = solved.len(),
        ndrawn = chosen.len(),
        floor = floor_deg / ARCSEC,
    );
    if let Some(dir) = args.out_tex.parent() {
        fs::create_dir_all(dir).ok();
    }
    fs::write(&args.out_tex, tex).expect("write tex");
    eprintln!("summary: {}", serde_json::to_string(&record["summary"]).unwrap());
}
