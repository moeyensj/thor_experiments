//! Figure data: THOR 2's gather for one test orbit (the THOR 2 counterpart of THOR 1's Figures 1 and 2).
//!
//! The run's own path, not a re-derivation. The July 2025 store is composed from its per-night shards the way `thor run
//! --obs-catalog` composes it (`SharedState::load_composed_shards`, the per-shard dataset mask, the observatory-code
//! mask by `bvh::selection::compose_row_selection`), the run config is loaded and its sigmas resolved against the test
//! orbit file's tiling (`THORConfig::resolve_sigmas`), the grid contract is applied to the test orbit
//! (`GridContract::apply`), observers are precomputed with ASSIST on the run config's IAS15 settings, built as `thor run
//! --backend assist` builds it (`propagator::assist_with_settings(&config.assist)`),
//! and the test orbit's sweep input is built by `thor::pipeline::sweep::sweep_input`: the usable arc, the BVH gather, the
//! tangent origin, the range-and-transform and the trail rule.
//!
//! The aperture is read from the same code. `sweep_input` does not return the gather's segments or guards, so the gather
//! core (`bvh::filter::gather_orbit_candidates`) is called again on exactly the inputs `stages::filter::run` and
//! `bvh_filter_observations` hand it (the usable arc's observer states, clipped to the query window), and its row set is
//! checked against the sweep input's; the per-segment guards are rebuilt by `bvh::guard::build_segment_guards` on those
//! segments, and every hit is checked to lie inside the guard of the segment that admitted it. For each gathered exposure
//! the covariance ellipse at that instant is THOR's guard for a zero-length segment at the predicted direction (no chord,
//! no site pad): the aperture before the segment's chord inflation.
//!
//! The known-object label is Rubin's ssObjectId and MPC packed designation, from THOR's labels table for the July store
//! (`thor labels build`, joined by obs_id). For one exposure (the panel-(a) rule, `chosen_k` below), every store
//! detection of that exposure inside twice its aperture is written, gathered or not.
//!
//! THE BACKEND is ASSIST, the search's own, for everything: the observer states, the usable arc, the gather (whose
//! segment route THOR resolves per orbit with `segments_need_nbody` under the config's `segment_propagator`; the guards are
//! rebuilt on the route it resolved) and the ephemeris the transform reads. Requires the crate's assist feature. The
//! result is checked against the r07dp2 run's own records: `checks.run_ids` (the gathered ids, from the gate's sweep
//! input) and `checks.run_bundle_theta` (the frame coordinates, from the run's bundle).
//!
//! Build: cargo build --release --features assist --bin fig_gather
//!
//! Usage:
//!   fig_gather --catalog PATH --dates LO,HI --config PATH --contract PATH --override-reason TEXT
//!              --test-orbits PATH [--orbit-id ID] --include-datasets A[,B] --labels PATH
//!              [--run-ids PATH] [--run-bundle PATH] [--exposure STATE_ID] --threads N --out-dir DIR --out-tex PATH
//!
//! The figure's run (2026-11-06, 16 threads on CPUs 44-59; E = thor-rust-experiments, G = E/search/step1/
//! sizing_contract_2026-11-03/gate1/run_side, T = f7445n32_r2.2249_b00n00_rdot00_e01of03), from the repository root:
//!   fig_gather --catalog $E/runs/step1/initial-run-r03dp2/local/catalog_20250630_20250730/catalog.json
//!     --dates 2025-06-30,2025-07-10 --config $G/configs/75104c17511f.json
//!     --contract $E/runs/step1/tiling-r09-july-full-solar-system-ladder/inner/f7445.ladder.e01of03.footprint4.contract.json
//!     --override-reason "<r07dp2 run.json grid_policy.reason>" --test-orbits $E/runs/step1/initial-run-r02/frames/$T.parquet
//!     --include-datasets lsst_dp2 --labels $E/runs/step1/full-run-r01dp2/campaign/labels/labels.parquet
//!     --run-ids $G/B1000/$T.ids
//!     --run-bundle $E/runs/step1/initial-run-r07dp2/results/$T/bundles/7445/$T/transformed_detections.parquet
//!     --threads 16 --out-dir experiments/results/fig_gather --out-tex generated/fig_gather.tex
//! The catalog composes the July store's night shards (/var/tmp/jul_hp128) with the 2025-06-30 shard the window needs;
//! the config is the r07dp2 job's own with the one field this engine does not know removed (the gate's derive_config.py).
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use arrow::array::{
    Array, ArrayRef, BooleanArray, Float32Array, Float64Array, Int16Array, Int64Array, StringArray, UInt64Array,
};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::{ArrowWriter, ProjectionMask};
use parquet::basic::{Compression, Encoding, ZstdLevel};
use parquet::file::properties::WriterProperties;
use parquet::schema::types::ColumnPath;
use rayon::prelude::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use thor::bvh::config::GuardConfig;
use thor::bvh::filter::{gather_orbit_candidates, GatherFilters};
use thor::bvh::guard::{build_segment_guards, build_segment_guards_nbody, OrientedGuard};
use thor::bvh::segments::{observer_arc_days, segments_need_nbody, OrbitSegment};
use thor::observations::{ObsStore, StringInterner, TrailStore};
use thor::pipeline::catalog::{parse_dates_arg, ThorObsCatalog};
use thor::pipeline::shared::SharedState;
use thor::pipeline::sweep::{sweep_input, SweepInputOutcome};
use thor::pipeline::THORConfig;
use thor::propagator::{assist_settings_line, assist_with_settings, Frame, ObserverState, TestOrbit};
use thor::test_orbits::contract::GridContract;

const RAD2DEG: f64 = 180.0 / std::f64::consts::PI;
const DEG2RAD: f64 = std::f64::consts::PI / 180.0;

struct Args {
    catalog: PathBuf,
    dates: String,
    config: PathBuf,
    contract: PathBuf,
    override_reason: String,
    test_orbits: PathBuf,
    orbit_id: Option<String>,
    include_datasets: Vec<String>,
    labels: PathBuf,
    run_ids: Option<PathBuf>,
    run_bundle: Option<PathBuf>,
    exposure: Option<u64>,
    threads: usize,
    out_dir: PathBuf,
    out_tex: PathBuf,
}

fn parse_args() -> Args {
    let mut a = std::env::args().skip(1);
    let mut m: HashMap<String, String> = HashMap::new();
    while let Some(k) = a.next() {
        let v = a.next().unwrap_or_else(|| panic!("{k} needs a value"));
        m.insert(k, v);
    }
    let take = |m: &mut HashMap<String, String>, k: &str| m.remove(k);
    let need = |m: &mut HashMap<String, String>, k: &str| take(m, k).unwrap_or_else(|| panic!("{k} is required"));
    let args = Args {
        catalog: PathBuf::from(need(&mut m, "--catalog")),
        dates: need(&mut m, "--dates"),
        config: PathBuf::from(need(&mut m, "--config")),
        contract: PathBuf::from(need(&mut m, "--contract")),
        override_reason: need(&mut m, "--override-reason"),
        test_orbits: PathBuf::from(need(&mut m, "--test-orbits")),
        orbit_id: take(&mut m, "--orbit-id"),
        include_datasets: need(&mut m, "--include-datasets").split(',').map(|s| s.trim().to_string()).collect(),
        labels: PathBuf::from(need(&mut m, "--labels")),
        run_ids: take(&mut m, "--run-ids").map(PathBuf::from),
        run_bundle: take(&mut m, "--run-bundle").map(PathBuf::from),
        exposure: take(&mut m, "--exposure").map(|s| s.parse().expect("--exposure is a state_id")),
        threads: need(&mut m, "--threads").parse().expect("--threads"),
        out_dir: PathBuf::from(need(&mut m, "--out-dir")),
        out_tex: PathBuf::from(need(&mut m, "--out-tex")),
    };
    if let Some(k) = m.keys().next() {
        panic!("unknown argument {k}");
    }
    args
}

fn sha256(path: &Path) -> String {
    let mut f = File::open(path).unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
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

fn input(path: &Path, role: &str) -> Value {
    json!({"role": role, "path": path.display().to_string(), "sha256": sha256(path)})
}

fn radec_of(d: [f64; 3]) -> (f64, f64) {
    let n = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    let ra = d[1].atan2(d[0]).rem_euclid(2.0 * std::f64::consts::PI) * RAD2DEG;
    let dec = (d[2] / n).clamp(-1.0, 1.0).asin() * RAD2DEG;
    (ra, dec)
}

fn unit(ra_deg: f64, dec_deg: f64) -> [f64; 3] {
    thor::linalg::radec_to_unit_vector(ra_deg, dec_deg)
}

/// The guard as written to the record: its own fields, plus the major axis's position angle on the sky (degrees east
/// of north). `los_triad` makes `e1` local east and `e2` local north for a direction off the pole, and `theta_rad` is
/// measured from `e1` toward `e2`, so the position angle is 90 deg - theta.
fn guard_json(g: &OrientedGuard) -> Value {
    let (ra, dec) = radec_of(g.center_dir);
    json!({
        "center_ra_deg": ra, "center_dec_deg": dec, "center_dir": g.center_dir, "e1": g.e1, "e2": g.e2,
        "semi_major_deg": g.semi_major_rad * RAD2DEG, "semi_minor_deg": g.semi_minor_rad * RAD2DEG,
        "theta_deg": g.theta_rad * RAD2DEG,
        "position_angle_deg": (90.0 - g.theta_rad * RAD2DEG).rem_euclid(180.0),
        "rho_min_au": g.rho_min, "rho_max_au": g.rho_max,
    })
}

fn quantile(v: &mut [f64], q: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(f64::total_cmp);
    let i = ((v.len() - 1) as f64 * q).round() as usize;
    v[i]
}

/// Thousands separators for LaTeX: 184355 -> 184{,}355.
fn tex_int(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push_str("{,}");
        }
        out.push(c);
    }
    out
}

/// One row of the output table.
struct Row {
    gathered: bool,
    ra: f64,
    dec: f64,
    time_utc: f64,
    night: i64,
    state_id: u64,
    seg_id: i16,
    theta: Option<(f64, f64)>,
    id: u128,
}

fn main() {
    let t_all = Instant::now();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).format_timestamp(None).init();
    let args = parse_args();
    rayon::ThreadPoolBuilder::new().num_threads(args.threads).build_global().expect("rayon pool");
    let mut timing: BTreeMap<&str, f64> = BTreeMap::new();

    // ── The run config, its sigmas resolved against the test-orbit file's tiling ───────────────────────────────────
    let raw = fs::read_to_string(&args.config).expect("config");
    let mut config: THORConfig = serde_json::from_str(&raw).expect("config parses as THORConfig");
    config.validate().expect("config validates");
    let (win_lo_utc, win_hi_utc) = match (config.bvh.query.min_mjd_utc, config.bvh.query.max_mjd_utc) {
        (Some(a), Some(b)) => (a, b),
        w => panic!("the config states no query window ({w:?}); the run's window is part of the record"),
    };
    assert!(
        config.bvh.morphology_gate.is_none(),
        "bvh.morphology_gate is set: the run would build a per-shard morphology resolver, which this experiment does not"
    );
    let tf = thor::test_orbits::read_test_orbits_parquet_with_span(&args.test_orbits).expect("test orbits");
    let orbit: TestOrbit = match &args.orbit_id {
        Some(id) => tf.orbits.iter().find(|o| &o.id == id).cloned().unwrap_or_else(|| panic!("no test orbit {id}")),
        None => {
            assert_eq!(tf.orbits.len(), 1, "the file holds several test orbits; pass --orbit-id");
            tf.orbits[0].clone()
        }
    };
    config.resolve_sigmas(tf.tiling_n_sigma, &args.test_orbits.display().to_string()).expect("resolve_sigmas");

    // ── The grid contract, as `thor run --grid-contract` loads it and `run_single_orbit` applies it ────────────────
    let gc = GridContract::load(&args.contract).expect("grid contract");
    let refusals = gc.check_contract_only(Some(&args.override_reason));
    assert!(refusals.is_empty(), "the contract refuses this run: {refusals:?}");
    config.grid_contract = Some(Arc::new(gc));

    // ── The store: the catalog's night shards for --dates, composed as `thor run --obs-catalog` composes them ──────
    let t = Instant::now();
    let (lo, hi) = parse_dates_arg(&args.dates).expect("--dates");
    let catalog = ThorObsCatalog::read(&args.catalog).expect("catalog");
    let catalog_dir = args.catalog.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let (shards, missing) = catalog.resolve_window_with_gaps(lo, hi, catalog_dir, false).expect("catalog window");
    assert!(missing.is_empty(), "days missing from the catalog: {missing:?}");
    let mut interner = StringInterner::new();
    let mut pairs = Vec::new();
    let mut masks = Vec::new();
    let mut shard_trails: Vec<Option<TrailStore>> = Vec::new();
    let mut shard_records = Vec::new();
    for s in &shards {
        let store = ObsStore::open(&s.store_path).expect("night-shard store");
        assert_eq!(
            store.manifest().ingest.healpix_nside,
            config.ingest.healpix_nside,
            "shard {} nside disagrees with the config",
            s.partition_date
        );
        for (local_id, code) in store.obs_code_table().iter().enumerate() {
            assert_eq!(interner.intern(code), local_id as u64, "shard {} obs-code ids diverge", s.partition_date);
        }
        shard_trails.push(store.open_trails().expect("trail sidecar"));
        masks.push(
            thor::observations::shard_selection_mask(&store, Some(&args.include_datasets), None, &s.partition_date)
                .expect("dataset selection"),
        );
        let manifest: Value = serde_json::from_reader(File::open(s.store_path.join("manifest.json")).expect("manifest"))
            .expect("manifest json");
        shard_records.push(json!({
            "partition_date": s.partition_date, "store": s.store_path.display().to_string(),
            "index": s.index_path.display().to_string(), "n_obs": store.len(),
            "obs_bin_xxh3_64": manifest["obs_bin"]["xxh3_64"], "obs_bin_bytes": manifest["obs_bin"]["bytes"],
        }));
        pairs.push((s.store_path.clone(), s.index_path.clone()));
    }
    let n_with_trails = shard_trails.iter().filter(|t| t.is_some()).count();
    let trails = if n_with_trails == 0 {
        None
    } else {
        assert_eq!(n_with_trails, shard_trails.len(), "a partial set of trail sidecars has no composed row space");
        let mut bases = Vec::new();
        let mut stores = Vec::new();
        let mut base = 0usize;
        for t in shard_trails.into_iter().flatten() {
            bases.push(base);
            base += t.len();
            stores.push(t);
        }
        Some(Arc::new(TrailStore::composed(stores, bases)))
    };
    let mut shared = SharedState::load_composed_shards(&pairs, interner.clone(), HashMap::new(), trails, Some(masks), None)
        .expect("compose the window's night shards");
    if let Some(mask) = shared.selection.as_deref() {
        mask.verify_requested_datasets_present().expect("requested datasets present");
    }
    let datasets_mask = shared.selection.take().map(Arc::unwrap_or_clone);
    let composed = thor::bvh::selection::compose_row_selection(
        &shared.observations,
        &shared.obs_code_interner,
        datasets_mask,
        config.bvh.query.observatory_codes.as_deref(),
        config.bvh.query.selection_combine,
    )
    .expect("row selection");
    shared.selection = composed.map(Arc::new);
    let n_store = shared.observations.len();
    let n_admitted = shared.selection.as_ref().map_or(n_store, |m| m.admitted());
    eprintln!("store: {} shards, {n_store} rows, {n_admitted} admitted by the selection", pairs.len());
    config.weighting.resolve_for_run(&shared.obs_code_interner).expect("weighting");
    timing.insert("compose_store_s", t.elapsed().as_secs_f64());

    // ── ASSIST, as `thor run --backend assist` builds it: the run config's IAS15 settings, the data manager's files ───
    let t = Instant::now();
    let propagator = assist_with_settings(&config.assist).expect("the ASSIST backend");
    let assist_settings = assist_settings_line(&propagator);
    eprintln!("ASSIST {assist_settings} (from config.assist)");
    timing.insert("load_assist_s", t.elapsed().as_secs_f64());
    let t = Instant::now();
    shared.precompute_observers(&propagator).expect("observers");
    timing.insert("observers_s", t.elapsed().as_secs_f64());
    let n_observers = shared.observer_states.as_ref().map_or(0, |s| s.len());

    // ── The test orbit's own config: the contract applied as run_single_orbit applies it ───────────────────────────
    let mut cfg = config.clone();
    let shell = config.grid_contract.as_ref().unwrap().apply(&mut cfg, &orbit.id).map(|s| (s.rho_lo_au, s.rho_hi_au));
    assert_eq!(
        cfg.clustering.trails,
        thor::clustering::config::TrailPolicy::Off,
        "the trail channel is armed; this experiment composes no morphology resolver for it"
    );

    let q = thor::test_orbits::elements::perihelion_au(&orbit.state);
    let regime = thor::kepler::classify_regime(&orbit.state);

    // ── The gather's own inputs, as stages::filter::run and bvh_filter_observations build them ───────────────────
    let ids = shared.observer_state_ids.as_ref().unwrap().clone();
    let states = shared.observer_states.as_ref().unwrap().clone();
    let arc = thor::pipeline::stages::setup::usable_arc(&orbit, &shared, &propagator, &cfg).expect("usable arc");
    let (arc_ids, arc_states): (Vec<u64>, Vec<ObserverState>) = match arc.as_ref() {
        Some(a) => ids
            .iter()
            .zip(states.iter())
            .filter(|(_, s)| s.state.epoch >= a.t_start && s.state.epoch <= a.t_end)
            .map(|(&i, s)| (i, s.clone()))
            .unzip(),
        None => (ids.clone(), states.clone()),
    };
    let (lo_tdb, hi_tdb) = (thor::time::utc_to_tdb(win_lo_utc), thor::time::utc_to_tdb(win_hi_utc));
    let w_states: Vec<ObserverState> =
        arc_states.iter().filter(|s| s.state.epoch >= lo_tdb && s.state.epoch <= hi_tdb).cloned().collect();
    let n_w_ids = arc_ids
        .iter()
        .zip(arc_states.iter())
        .filter(|(_, s)| s.state.epoch >= lo_tdb && s.state.epoch <= hi_tdb)
        .count();
    let (t_min, t_max) = w_states
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), s| (a.min(s.state.epoch), b.max(s.state.epoch)));
    let nbody = segments_need_nbody(&orbit, t_min, t_max, &cfg.bvh, &propagator).expect("segment route");
    let gather_route = if nbody { "n-body anchors (segments_need_nbody = true)" } else { "2-body segments (segments_need_nbody = false)" };
    eprintln!("gather route: {gather_route}");

    // ── THE SWEEP INPUT: the run's own stages from the usable arc to the trail rule ───────────────────────────────
    let t = Instant::now();
    let claimed = Arc::new(Mutex::new(HashSet::<u128>::new()));
    let mut rss = Vec::new();
    let sweep = match sweep_input(&orbit, &shared, &propagator, &cfg, &claimed, None, 0, &mut rss).expect("sweep_input") {
        SweepInputOutcome::Ready(s) => s,
        SweepInputOutcome::Exit { disposition, .. } => panic!("the run does not sweep this test orbit: {disposition:?}"),
    };
    timing.insert("sweep_input_s", t.elapsed().as_secs_f64());
    timing.insert("sweep_gather_ms", sweep.filter_ms as f64);
    timing.insert("sweep_transform_ms", sweep.transform_ms as f64);
    let fr = &sweep.filter_result;
    let gathered_rows: BTreeSet<usize> = fr.observation_indices.iter().copied().collect();
    eprintln!(
        "sweep input: {} gathered, {} transformed, {} exposures in the ephemeris, origin {:?}",
        gathered_rows.len(),
        sweep.detections.len(),
        fr.ephemeris.len(),
        sweep.tangent_origin
    );

    // ── The gather core again, on the same inputs, for its segments and per-hit segment ───────────────────────────
    let t = Instant::now();
    let bundle = shared.bvh_obs_index.as_ref().expect("composed index");
    let gather = gather_orbit_candidates(
        &orbit,
        &w_states,
        &propagator,
        &cfg.bvh,
        &bundle.0,
        None,
        &GatherFilters {
            observations: &shared.observations,
            selection: shared.selection.as_deref(),
            group_selection: shared.group_selection.as_deref(),
        },
    )
    .expect("gather core");
    let obs = &shared.observations;
    let mut retrace_rows: BTreeSet<usize> = BTreeSet::new();
    let mut seg_of_row: HashMap<usize, u32> = HashMap::new();
    for (row, hit) in &gather.hits {
        let o = obs.row(*row);
        if o.time_utc < win_lo_utc || o.time_utc > win_hi_utc {
            continue;
        }
        retrace_rows.insert(*row);
        seg_of_row.entry(*row).and_modify(|s| *s = (*s).min(hit.seg_id)).or_insert(hit.seg_id);
    }
    let retrace_identical = retrace_rows == gathered_rows;
    assert!(
        retrace_identical,
        "the re-traced gather ({} rows) differs from the sweep input's ({} rows)",
        retrace_rows.len(),
        gathered_rows.len()
    );
    let (n_sigma, cap_deg) = match cfg.bvh.query.guard() {
        GuardConfig::CovarianceOriented { n_sigma, max_radius_deg } => (n_sigma, max_radius_deg),
        g => panic!("the run's guard is not covariance-oriented: {g:?}"),
    };
    let arc_days = observer_arc_days(&w_states);
    // The gather traces one orbit with local orbit id 0 (`generate_segments_with_tier(test_orbit, 0, ..)`).
    assert!(gather.segments.iter().all(|s| s.orbit_id == 0), "segments of more than one local orbit");
    // The guards on the route the gather resolved, as gather_orbit_candidates builds them.
    let segment_guards = |segs: &[OrbitSegment]| {
        if nbody {
            build_segment_guards_nbody(&orbit, segs, n_sigma, &propagator).expect("n-body segment guards")
        } else {
            let (g, needs_nbody) = build_segment_guards(&orbit, segs, n_sigma, arc_days).expect("segment guards");
            assert!(!needs_nbody, "build_segment_guards refuses the 2-body route the gather resolved");
            g
        }
    };
    let mut guards = segment_guards(&gather.segments);
    if let Some(cap) = cap_deg {
        for g in guards.values_mut() {
            g.cap_angular(cap * DEG2RAD);
        }
    }
    // Every hit must lie inside the guard of the segment that admitted it (centroid test; a trailed detection is a
    // chord leaf the index admits on either endpoint, so its centroid may lie outside).
    let mut n_hits_outside = 0usize;
    for (row, hit) in &gather.hits {
        let o = obs.row(*row);
        let g = &guards[&(hit.orbit_id, hit.seg_id)];
        if !g.contains_dir(unit(o.ra, o.dec)) {
            n_hits_outside += 1;
        }
    }
    timing.insert("retrace_s", t.elapsed().as_secs_f64());
    let mut seg_hits: HashMap<u32, usize> = HashMap::new();
    for s in seg_of_row.values() {
        *seg_hits.entry(*s).or_default() += 1;
    }
    let segments_json: Vec<Value> = gather
        .segments
        .iter()
        .map(|s: &OrbitSegment| {
            let g = &guards[&(s.orbit_id, s.seg_id)];
            let (ra0, dec0) = radec_of(s.dir_start);
            let (ra1, dec1) = radec_of(s.dir_end);
            let (rac, decc) = radec_of(s.center_dir);
            json!({
                "seg_id": s.seg_id, "t_start_utc": s.t_start_utc, "t_end_utc": s.t_end_utc,
                "start_ra_deg": ra0, "start_dec_deg": dec0, "end_ra_deg": ra1, "end_dec_deg": dec1,
                "center_ra_deg": rac, "center_dec_deg": decc, "rho_min_au": s.rho_min, "rho_max_au": s.rho_max,
                "site_pad_arcsec": s.site_pad_rad * RAD2DEG * 3600.0,
                "half_chord_deg": 0.5 * thor::bvh::segments::angular_distance(s.dir_start, s.dir_end) * RAD2DEG,
                "detections_first_admitted": seg_hits.get(&s.seg_id).copied().unwrap_or(0),
                "guard": guard_json(g),
            })
        })
        .collect();

    // ── Per gathered exposure: time, predicted position, and the covariance ellipse at that instant ───────────────
    let mut exp_rows: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
    for &r in &gathered_rows {
        exp_rows.entry(obs.row(r).state_id).or_default().push(r);
    }
    let mut exposures: Vec<(u64, f64, i64)> = exp_rows
        .iter()
        .map(|(sid, rows)| (*sid, obs.row(rows[0]).time_utc, obs.row(rows[0]).night))
        .collect();
    exposures.sort_by(|a, b| a.1.total_cmp(&b.1));
    let mut zero_segments = Vec::new();
    let mut predicted: HashMap<u64, (f64, f64, f64)> = HashMap::new();
    for (k, (sid, t_utc, _)) in exposures.iter().enumerate() {
        let eph = fr.ephemeris.get(sid).unwrap_or_else(|| panic!("no ephemeris for exposure {sid}"));
        assert!(matches!(eph.state.frame, Frame::Equatorial), "ephemeris frame {:?}", eph.state.frame);
        let (ra, dec, rho) = (eph.state.lon, eph.state.lat, eph.state.rho);
        predicted.insert(*sid, (ra, dec, rho));
        let d = unit(ra, dec);
        zero_segments.push(OrbitSegment {
            orbit_id: 0,
            seg_id: k as u32,
            t_start_utc: *t_utc,
            t_end_utc: *t_utc,
            dir_start: d,
            dir_end: d,
            center_dir: d,
            rho_min: rho,
            rho_max: rho,
            site_pad_rad: 0.0,
        });
    }
    let mut zero_guards = segment_guards(&zero_segments);
    if let Some(cap) = cap_deg {
        for g in zero_guards.values_mut() {
            g.cap_angular(cap * DEG2RAD);
        }
    }
    let segs_at = |t_utc: f64| -> Vec<u32> {
        gather.segments.iter().filter(|s| t_utc >= s.t_start_utc && t_utc <= s.t_end_utc).map(|s| s.seg_id).collect()
    };

    // ── The store around each gathered exposure's aperture: one pass over every row ───────────────────────────────
    let t = Instant::now();
    let exp_index: HashMap<u64, usize> = exposures.iter().enumerate().map(|(k, e)| (e.0, k)).collect();
    let exp_guards: Vec<Vec<(OrientedGuard, OrientedGuard)>> = exposures
        .iter()
        .map(|(_, t_utc, _)| {
            segs_at(*t_utc)
                .into_iter()
                .map(|sid| {
                    let g = guards[&(0, sid)].clone();
                    let mut g2 = g.clone();
                    g2.semi_major_rad *= 2.0;
                    g2.semi_minor_rad *= 2.0;
                    (g, g2)
                })
                .collect()
        })
        .collect();
    let selection = shared.selection.clone();
    // (row, exposure index, inside the aperture, admitted by the selection)
    let near: Vec<(usize, usize, bool, bool)> = (0..n_store)
        .into_par_iter()
        .filter_map(|i| {
            let o = obs.row(i);
            let k = *exp_index.get(&o.state_id)?;
            let d = unit(o.ra, o.dec);
            let gs = &exp_guards[k];
            if !gs.iter().any(|(_, g2)| g2.contains_dir(d)) {
                return None;
            }
            let inside = gs.iter().any(|(g, _)| g.contains_dir(d));
            let admitted = selection.as_ref().is_none_or(|m| m.admits(i));
            Some((i, k, inside, admitted))
        })
        .collect();
    timing.insert("store_scan_s", t.elapsed().as_secs_f64());
    let mut per_exp: Vec<BTreeMap<&str, usize>> = vec![BTreeMap::new(); exposures.len()];
    for (i, k, inside, admitted) in &near {
        let gathered = gathered_rows.contains(i);
        let key = match (gathered, inside, admitted) {
            (true, _, _) => "gathered_in_2x",
            (false, true, true) => "not_gathered_inside_aperture",
            (false, false, true) => "not_gathered_in_2x",
            (false, _, false) => "not_admitted_in_2x",
        };
        *per_exp[*k].entry(key).or_default() += 1;
    }
    // THE PANEL-(a) EXPOSURE: the one that shows both sides of the aperture best, i.e. the largest
    // min(gathered detections, admitted detections within twice the aperture that were not gathered).
    let chosen_k = match args.exposure {
        Some(sid) => *exp_index.get(&sid).expect("--exposure is not a gathered exposure"),
        None => (0..exposures.len())
            .max_by_key(|&k| {
                let g = exp_rows[&exposures[k].0].len();
                let n = per_exp[k].get("not_gathered_in_2x").copied().unwrap_or(0);
                (g.min(n), g)
            })
            .unwrap(),
    };
    let chosen_sid = exposures[chosen_k].0;
    let n_inside_not_gathered: usize =
        per_exp.iter().map(|m| m.get("not_gathered_inside_aperture").copied().unwrap_or(0)).sum();

    // ── Rows: every gathered detection, and the chosen exposure's other admitted detections within 2x ─────────────
    let theta: HashMap<u128, (f64, f64)> =
        sweep.detections.iter().map(|d| (d.id, (d.coords.theta_x, d.coords.theta_y))).collect();
    let mut rows: Vec<Row> = Vec::with_capacity(gathered_rows.len() + 4096);
    for &r in &gathered_rows {
        let o = obs.row(r);
        rows.push(Row {
            gathered: true,
            ra: o.ra,
            dec: o.dec,
            time_utc: o.time_utc,
            night: o.night,
            state_id: o.state_id,
            seg_id: seg_of_row[&r] as i16,
            theta: theta.get(&o.id).copied(),
            id: o.id,
        });
    }
    for (i, k, _, admitted) in &near {
        if *k != chosen_k || !*admitted || gathered_rows.contains(i) {
            continue;
        }
        let o = obs.row(*i);
        rows.push(Row {
            gathered: false,
            ra: o.ra,
            dec: o.dec,
            time_utc: o.time_utc,
            night: o.night,
            state_id: o.state_id,
            seg_id: -1,
            theta: None,
            id: o.id,
        });
    }
    let n_transform_dropped = rows.iter().filter(|r| r.gathered && r.theta.is_none()).count();

    // ── Known-object labels: Rubin's ssObjectId and MPC designation, by obs_id ────────────────────────────────────
    let t = Instant::now();
    let want: HashMap<String, usize> = rows.iter().enumerate().map(|(k, r)| (format!("{:032x}", r.id), k)).collect();
    let mut label: Vec<Option<(String, Option<String>)>> = vec![None; rows.len()];
    {
        let file = File::open(&args.labels).expect("labels");
        let builder = ParquetRecordBatchReaderBuilder::try_new(file).expect("labels parquet");
        let schema = builder.parquet_schema().clone();
        let cols: Vec<usize> = ["det_id", "object_id", "designation"]
            .iter()
            .map(|c| schema.columns().iter().position(|x| x.name() == *c).unwrap_or_else(|| panic!("labels lack {c}")))
            .collect();
        let reader = builder.with_projection(ProjectionMask::leaves(&schema, cols)).build().expect("labels reader");
        for batch in reader {
            let b = batch.expect("labels batch");
            let det = b.column_by_name("det_id").unwrap().as_any().downcast_ref::<StringArray>().unwrap().clone();
            let oid = b.column_by_name("object_id").unwrap().as_any().downcast_ref::<StringArray>().unwrap().clone();
            let des = b.column_by_name("designation").unwrap().as_any().downcast_ref::<StringArray>().unwrap().clone();
            for i in 0..b.num_rows() {
                if let Some(&k) = want.get(det.value(i)) {
                    let d = (!des.is_null(i)).then(|| des.value(i).to_string());
                    label[k] = Some((oid.value(i).to_string(), d));
                }
            }
        }
    }
    timing.insert("labels_s", t.elapsed().as_secs_f64());
    let mut obj_dets: HashMap<&str, usize> = HashMap::new();
    let mut obj_nights: HashMap<&str, HashSet<i64>> = HashMap::new();
    for (r, l) in rows.iter().zip(label.iter()) {
        if let (true, Some((o, _))) = (r.gathered, l) {
            *obj_dets.entry(o.as_str()).or_default() += 1;
            obj_nights.entry(o.as_str()).or_default().insert(r.night);
        }
    }
    let n_known_dets: usize = obj_dets.values().sum();
    let n_objects = obj_dets.len();
    let n_objects_6on4 = obj_dets.iter().filter(|(o, n)| **n >= 6 && obj_nights[*o].len() >= 4).count();

    // ── Checks against the run's own records ─────────────────────────────────────────────────────────────────────
    let mut sweep_ids: Vec<String> = sweep.detections.iter().map(|d| format!("{:032x}", d.id)).collect();
    sweep_ids.sort_unstable();
    let ids_digest = {
        let mut h = Sha256::new();
        for s in &sweep_ids {
            h.update(s.as_bytes());
            h.update(b"\n");
        }
        format!("{:x}", h.finalize())
    };
    let run_ids_check = args.run_ids.as_ref().map(|p| {
        let text = fs::read_to_string(p).expect("run ids");
        let theirs: BTreeSet<&str> = text.split_whitespace().collect();
        let ours: BTreeSet<&str> = sweep_ids.iter().map(|s| s.as_str()).collect();
        json!({
            "path": p.display().to_string(), "sha256": sha256(p), "n_run": theirs.len(), "n_here": ours.len(),
            "identical": theirs == ours, "only_run": theirs.difference(&ours).count(),
            "only_here": ours.difference(&theirs).count(),
        })
    });
    let bundle_check = args.run_bundle.as_ref().map(|p| {
        let file = File::open(p).expect("run bundle");
        let reader = ParquetRecordBatchReaderBuilder::try_new(file).expect("bundle parquet").build().expect("reader");
        let by_hex: HashMap<String, (f64, f64)> =
            sweep.detections.iter().map(|d| (format!("{:032x}", d.id), (d.coords.theta_x, d.coords.theta_y))).collect();
        let (mut n_run, mut n_matched) = (0usize, 0usize);
        let mut sep = Vec::new();
        for batch in reader {
            let b = batch.expect("bundle batch");
            let id = b.column_by_name("obs_id").unwrap().as_any().downcast_ref::<StringArray>().unwrap().clone();
            let tx = b.column_by_name("theta_x").unwrap().as_any().downcast_ref::<Float64Array>().unwrap().clone();
            let ty = b.column_by_name("theta_y").unwrap().as_any().downcast_ref::<Float64Array>().unwrap().clone();
            for i in 0..b.num_rows() {
                n_run += 1;
                if let Some((x, y)) = by_hex.get(id.value(i)) {
                    n_matched += 1;
                    sep.push((x - tx.value(i)).hypot(y - ty.value(i)) * 3600.0);
                }
            }
        }
        json!({
            "path": p.display().to_string(), "sha256": sha256(p), "n_run": n_run, "n_matched": n_matched,
            "theta_difference_arcsec": {
                "median": quantile(&mut sep.clone(), 0.5), "p99": quantile(&mut sep.clone(), 0.99),
                "max": quantile(&mut sep, 1.0),
            },
        })
    });

    // ── The table: a compressed parquet, angles in float32 (byte-stream-split), exposure columns dictionary-coded ─
    fs::create_dir_all(&args.out_dir).expect("out dir");
    let schema = Arc::new(Schema::new(vec![
        Field::new("gathered", DataType::Boolean, false),
        Field::new("ra", DataType::Float32, false),
        Field::new("dec", DataType::Float32, false),
        Field::new("time_utc", DataType::Float64, false),
        Field::new("night", DataType::Int64, false),
        Field::new("state_id", DataType::UInt64, false),
        Field::new("seg_id", DataType::Int16, false),
        Field::new("theta_x", DataType::Float32, true),
        Field::new("theta_y", DataType::Float32, true),
        Field::new("ss_object_id", DataType::Utf8, true),
        Field::new("designation", DataType::Utf8, true),
    ]));
    // Gathered rows first in time order, then the chosen exposure's neighbours.
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by(|&a, &b| {
        (!rows[a].gathered, rows[a].time_utc, rows[a].id).partial_cmp(&(!rows[b].gathered, rows[b].time_utc, rows[b].id)).unwrap()
    });
    let cols: Vec<ArrayRef> = vec![
        Arc::new(BooleanArray::from(order.iter().map(|&k| rows[k].gathered).collect::<Vec<_>>())),
        Arc::new(Float32Array::from(order.iter().map(|&k| rows[k].ra as f32).collect::<Vec<_>>())),
        Arc::new(Float32Array::from(order.iter().map(|&k| rows[k].dec as f32).collect::<Vec<_>>())),
        Arc::new(Float64Array::from(order.iter().map(|&k| rows[k].time_utc).collect::<Vec<_>>())),
        Arc::new(Int64Array::from(order.iter().map(|&k| rows[k].night).collect::<Vec<_>>())),
        Arc::new(UInt64Array::from(order.iter().map(|&k| rows[k].state_id).collect::<Vec<_>>())),
        Arc::new(Int16Array::from(order.iter().map(|&k| rows[k].seg_id).collect::<Vec<_>>())),
        Arc::new(Float32Array::from(order.iter().map(|&k| rows[k].theta.map(|t| t.0 as f32)).collect::<Vec<_>>())),
        Arc::new(Float32Array::from(order.iter().map(|&k| rows[k].theta.map(|t| t.1 as f32)).collect::<Vec<_>>())),
        Arc::new(StringArray::from(order.iter().map(|&k| label[k].as_ref().map(|l| l.0.clone())).collect::<Vec<_>>())),
        Arc::new(StringArray::from(
            order.iter().map(|&k| label[k].as_ref().and_then(|l| l.1.clone())).collect::<Vec<_>>(),
        )),
    ];
    let batch = RecordBatch::try_new(schema.clone(), cols).expect("record batch");
    let mut props =
        WriterProperties::builder().set_compression(Compression::ZSTD(ZstdLevel::try_new(19).unwrap()));
    for c in ["ra", "dec", "theta_x", "theta_y"] {
        props = props
            .set_column_dictionary_enabled(ColumnPath::from(c), false)
            .set_column_encoding(ColumnPath::from(c), Encoding::BYTE_STREAM_SPLIT);
    }
    let pq_path = args.out_dir.join("detections.parquet");
    {
        let mut w = ArrowWriter::try_new(File::create(&pq_path).expect("parquet out"), schema, Some(props.build()))
            .expect("parquet writer");
        w.write(&batch).expect("write");
        w.close().expect("close");
    }

    // ── The record ────────────────────────────────────────────────────────────────────────────────────────────────
    let s = &orbit.state;
    let cov = s.covariance.expect("covariance");
    let rh = (s.x * s.x + s.y * s.y + s.z * s.z).sqrt();
    let mu = 2.959_122_082_855_911_5e-4;
    let v2 = s.vx * s.vx + s.vy * s.vy + s.vz * s.vz;
    let a_au = 1.0 / (2.0 / rh - v2 / mu);
    let (hx, hy, hz) = (s.y * s.vz - s.z * s.vy, s.z * s.vx - s.x * s.vz, s.x * s.vy - s.y * s.vx);
    let h = (hx * hx + hy * hy + hz * hz).sqrt();
    let e = (1.0 - h * h / (mu * a_au)).max(0.0).sqrt();
    let inc = (hz / h).acos() * RAD2DEG;
    let rhos: Vec<f64> = exposures.iter().map(|e| predicted[&e.0].2).collect();
    let rho_mean = rhos.iter().sum::<f64>() / rhos.len() as f64;
    let nights: BTreeSet<i64> = exposures.iter().map(|e| e.2).collect();
    let exposures_json: Vec<Value> = exposures
        .iter()
        .enumerate()
        .map(|(k, (sid, t_utc, night))| {
            let (ra, dec, rho) = predicted[sid];
            let mut counts = per_exp[k].clone();
            counts.insert("gathered", exp_rows[sid].len());
            json!({
                "state_id": sid.to_string(), "time_utc": t_utc, "epoch_tdb": fr.ephemeris[sid].state.epoch,
                "night": night, "predicted_ra_deg": ra, "predicted_dec_deg": dec, "predicted_rho_au": rho,
                "segments": segs_at(*t_utc), "counts": counts, "epoch_guard": guard_json(&zero_guards[&(0, k as u32)]),
            })
        })
        .collect();
    let semi_major: Vec<f64> = gather.segments.iter().map(|s| guards[&(s.orbit_id, s.seg_id)].semi_major_rad * RAD2DEG).collect();
    let semi_minor: Vec<f64> = gather.segments.iter().map(|s| guards[&(s.orbit_id, s.seg_id)].semi_minor_rad * RAD2DEG).collect();
    let epoch_major: Vec<f64> = zero_guards.values().map(|g| g.semi_major_rad * RAD2DEG).collect();
    let epoch_minor: Vec<f64> = zero_guards.values().map(|g| g.semi_minor_rad * RAD2DEG).collect();
    let fmin = |v: &[f64]| v.iter().copied().fold(f64::INFINITY, f64::min);
    let fmax = |v: &[f64]| v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let chosen = &exposures_json[chosen_k];
    let chosen_not_gathered = per_exp[chosen_k].get("not_gathered_in_2x").copied().unwrap_or(0);

    // The files ASSIST's data manager resolved (assist_rs::data: $ASSIST_DATA_DIR, else $XDG_CACHE_HOME/assist-rs, else
    // ~/.cache/assist-rs; its DEFAULT_KERNELS), by SHA-256.
    let assist_dir = std::env::var("ASSIST_DATA_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var("XDG_CACHE_HOME").ok().filter(|s| !s.is_empty()).map(|x| Path::new(&x).join("assist-rs")))
        .unwrap_or_else(|| Path::new(&std::env::var("HOME").unwrap_or_default()).join(".cache/assist-rs"));
    let assist_files: Vec<Value> = [
        ("de440.bsp", "planetary ephemeris"),
        ("sb441-n16.bsp", "asteroid perturbers"),
        ("obscodes_extended.json", "MPC observatory table"),
        ("earth_latest_high_prec.bpc", "Earth orientation (high precision)"),
        ("earth_620120_250826.bpc", "Earth orientation (historical)"),
        ("earth_2025_250826_2125_predict.bpc", "Earth orientation (predict)"),
    ]
    .iter()
    .map(|(f, role)| input(&assist_dir.join(f), role))
    .collect();

    let cpu = fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("model name")).map(|l| l.split(':').nth(1).unwrap_or("").trim().to_string()))
        .unwrap_or_default();
    let cpus_allowed = fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("Cpus_allowed_list")).map(|l| l.split(':').nth(1).unwrap_or("").trim().to_string()))
        .unwrap_or_default();
    timing.insert("total_s", t_all.elapsed().as_secs_f64());
    let record = json!({
        "experiment": "fig_gather",
        "thor_pin": env!("THOR_PIN"), "thor_source": env!("THOR_SOURCE"),
        "backend": {
            "name": "ASSIST (thor-rs-assist; built as thor run --backend assist builds it)",
            "crates": env!("ASSIST_CRATES").split(';').collect::<Vec<_>>(),
            "integrator": assist_settings, "integrator_config": serde_json::to_value(&config.assist).unwrap(),
            "data_files": assist_files,
            "gather_route": gather_route,
            "segment_propagator": format!("{:?}", cfg.bvh.index.segment_propagator),
            "regime": format!("{regime:?}"),
        },
        "threads": args.threads, "cpus_allowed": cpus_allowed, "cpu": cpu,
        "inputs": {
            "catalog": input(&args.catalog, "thor.obs catalog (night shards)"), "dates": args.dates,
            "shards": shard_records,
            "config": input(&args.config, "run config"),
            "contract": input(&args.contract, "grid contract"), "override_reason": args.override_reason,
            "test_orbits": input(&args.test_orbits, "test orbit file"),
            "include_datasets": args.include_datasets,
            "labels": input(&args.labels, "known-object labels (thor.labels/v1, Rubin ssObjectId)"),
        },
        "test_orbit": {
            "id": orbit.id, "frame": format!("{:?}", s.frame), "origin": format!("{:?}", s.origin), "epoch_tdb": s.epoch,
            "state": [s.x, s.y, s.z, s.vx, s.vy, s.vz], "covariance": cov,
            "tiling_n_sigma": tf.tiling_n_sigma, "r_helio_au": rh, "a_au": a_au, "e": e, "i_deg": inc, "q_au": q,
            "rho_mean_au": rho_mean, "rho_min_au": fmin(&rhos), "rho_max_au": fmax(&rhos),
            "contract_shell_au": shell,
        },
        "run": {
            "query_window_utc": [win_lo_utc, win_hi_utc], "store_rows": n_store, "admitted_rows": n_admitted,
            "observers": n_observers, "usable_arc_tdb": arc.as_ref().map(|a| [a.t_start, a.t_end]),
            "gather_observer_states": n_w_ids, "arc_days": arc_days,
            "guard": {"kind": "CovarianceOriented", "n_sigma": n_sigma, "max_radius_deg": cap_deg,
                       "chord_sigma_fraction": cfg.bvh.index.chord_sigma_fraction},
            "tangent_origin": format!("{:?}", sweep.tangent_origin),
            "gathered": gathered_rows.len(), "transformed": sweep.detections.len(),
            "transform_dropped": n_transform_dropped, "exposures": exposures.len(), "nights": nights.len(),
            "known_detections": n_known_dets, "known_objects": n_objects,
            "known_objects_6_dets_4_nights": n_objects_6on4,
        },
        "segments": segments_json,
        "exposures": exposures_json,
        "panel_a_exposure": {
            "state_id": chosen_sid.to_string(), "index": chosen_k,
            "rule": "the gathered exposure with the largest min(gathered detections, admitted detections within twice \
                     the aperture that were not gathered)",
            "gathered": exp_rows[&chosen_sid].len(), "not_gathered_within_2x": chosen_not_gathered,
            "time_utc": chosen["time_utc"], "night": chosen["night"],
        },
        "checks": {
            "retrace_rows_identical_to_sweep_input": retrace_identical,
            "hits_outside_their_segment_guard": n_hits_outside, "hits": gather.hits.len(),
            "admitted_not_gathered_inside_an_aperture": n_inside_not_gathered,
            "sweep_input_ids_sha256": ids_digest,
            "run_ids": run_ids_check,
            "run_bundle_theta": bundle_check,
        },
        "table": {
            "path": pq_path.file_name().unwrap().to_string_lossy(), "rows": rows.len(),
            "columns": "gathered, ra, dec (deg, float32), time_utc (MJD UTC), night, state_id (exposure), seg_id \
                        (lowest segment whose guard admitted it; -1 not gathered), theta_x, theta_y (deg, float32, the \
                        transform's tangent-plane coordinates; null when not transformed), ss_object_id, designation",
            "sha256": sha256(&pq_path),
        },
        "timing": timing,
    });
    let json_path = args.out_dir.join("fig_gather.json");
    fs::write(&json_path, serde_json::to_string_pretty(&record).unwrap()).expect("write json");

    // ── Numbers for the caption, as LaTeX macros: never typed by hand ─────────────────────────────────────────────
    let pin = &env!("THOR_PIN")[..8.min(env!("THOR_PIN").len())];
    let window_days = win_hi_utc - win_lo_utc;
    let tex = format!(
        "%% Generated by experiments/src/bin/fig_gather.rs (THOR {pin}); do not edit.\n\
         \\newcommand{{\\fgRho}}{{{rho:.2}}}\n\
         \\newcommand{{\\fgRhoMin}}{{{rhomin:.2}}}\n\
         \\newcommand{{\\fgRhoMax}}{{{rhomax:.2}}}\n\
         \\newcommand{{\\fgRhelio}}{{{rh:.2}}}\n\
         \\newcommand{{\\fgWindowDays}}{{{wd:.1}}}\n\
         \\newcommand{{\\fgExposures}}{{{nexp}}}\n\
         \\newcommand{{\\fgNights}}{{{nn}}}\n\
         \\newcommand{{\\fgSegments}}{{{nseg}}}\n\
         \\newcommand{{\\fgGuardNSigma}}{{{ns:.0}}}\n\
         \\newcommand{{\\fgSemiMajorMin}}{{{smaj0:.1}}}\n\
         \\newcommand{{\\fgSemiMajorMax}}{{{smaj1:.1}}}\n\
         \\newcommand{{\\fgSemiMinorMin}}{{{smin0:.1}}}\n\
         \\newcommand{{\\fgSemiMinorMax}}{{{smin1:.1}}}\n\
         \\newcommand{{\\fgEpochSemiMajorMin}}{{{emaj0:.1}}}\n\
         \\newcommand{{\\fgEpochSemiMajorMax}}{{{emaj1:.1}}}\n\
         \\newcommand{{\\fgEpochSemiMinorMin}}{{{emin0:.1}}}\n\
         \\newcommand{{\\fgEpochSemiMinorMax}}{{{emin1:.1}}}\n\
         \\newcommand{{\\fgStoreMillions}}{{{sm:.1}}}\n\
         \\newcommand{{\\fgAdmittedMillions}}{{{am:.1}}}\n\
         \\newcommand{{\\fgGathered}}{{{ng}}}\n\
         \\newcommand{{\\fgKnownDetections}}{{{nk}}}\n\
         \\newcommand{{\\fgKnownObjects}}{{{no}}}\n\
         \\newcommand{{\\fgPanelAGathered}}{{{pag}}}\n\
         \\newcommand{{\\fgPanelANotGathered}}{{{pan}}}\n",
        rho = rho_mean, rhomin = fmin(&rhos), rhomax = fmax(&rhos), rh = rh, wd = window_days,
        nexp = exposures.len(), nn = nights.len(), nseg = gather.segments.len(), ns = n_sigma,
        smaj0 = fmin(&semi_major), smaj1 = fmax(&semi_major), smin0 = fmin(&semi_minor), smin1 = fmax(&semi_minor),
        emaj0 = fmin(&epoch_major), emaj1 = fmax(&epoch_major), emin0 = fmin(&epoch_minor), emin1 = fmax(&epoch_minor),
        sm = n_store as f64 / 1e6, am = n_admitted as f64 / 1e6,
        ng = tex_int(gathered_rows.len()), nk = tex_int(n_known_dets), no = tex_int(n_objects),
        pag = tex_int(exp_rows[&chosen_sid].len()), pan = tex_int(chosen_not_gathered),
    );
    if let Some(dir) = args.out_tex.parent() {
        fs::create_dir_all(dir).ok();
    }
    fs::write(&args.out_tex, tex).expect("write tex");
    eprintln!(
        "fig_gather: {} gathered ({} transformed) over {} exposures / {} nights, {} segments, {} known objects; \
         table {} rows; {:.0} s",
        gathered_rows.len(),
        sweep.detections.len(),
        exposures.len(),
        nights.len(),
        gather.segments.len(),
        n_objects,
        rows.len(),
        t_all.elapsed().as_secs_f64()
    );
}
