//! Clustering speed: Hotspot2D against DBSCAN, on the windows test orbits actually search.
//!
//! Input: window dumps written by THOR's sizing tiler (`sizing_tile`, "dump each accepted region's windows as the sweep
//! filter saw them"). Each region has a JSON (its windows: radius, rule, velocity covariance, window length) and a
//! Parquet of its windows' detections in the test orbit's frame, flagged `kept` where the sweep filter keeps them.
//!
//! For every window searched over velocities (rule of at least three detections), this takes the kept detections,
//! lays the velocity hypotheses with THOR's own generator (`generate_elliptical_velocity_grid`) over the window's
//! velocity covariance at THOR's envelope of n = 3 standard deviations, with step R / W (the step at which, in the
//! clustering model, neighbouring hypotheses stop re-finding the same groups), and times `ClusteringBackend::cluster`
//! with Hotspot2D (cell side R, k distinct exposures) and with DBSCAN (radius R, k samples) on identical input, in a
//! rayon pool of the requested size. It records wall time per repeat, the clusters returned, and the distinct member
//! sets among them.
//!
//! Usage:
//!   clustering_speed --dumps DIR --threads N --reps R --out-json PATH --out-tex PATH
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;

use arrow::array::{Array, BooleanArray, Float64Array, Int64Array, UInt32Array, UInt64Array};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use thor::clustering::adaptive::{generate_elliptical_velocity_grid, AdaptiveClusteringConfig};
use thor::clustering::{CPUClusteringBackend, ClusteringAlgorithm, ClusteringBackend};
use thor::pipeline::THORConfig;
use thor::projection::GnomonicCoords;
use thor::propagator::Origin;
use thor::transform::TransformedDetection;

/// THOR's velocity envelope: the hypotheses span this many standard deviations of the window's rate covariance
/// (`adaptive.velocity_envelope_n_sigma`, defaulting to the grid's `thor.cov_half_width_n_sigma` = 3).
const ENVELOPE_N_SIGMA: f64 = 3.0;

struct Args {
    dumps: PathBuf,
    threads: usize,
    reps: usize,
    out_json: PathBuf,
    out_tex: PathBuf,
}

fn parse_args() -> Args {
    let mut a = std::env::args().skip(1);
    let (mut dumps, mut threads, mut reps, mut out_json, mut out_tex) = (None, 1, 3, None, None);
    while let Some(k) = a.next() {
        let v = a.next().unwrap_or_else(|| panic!("{k} needs a value"));
        match k.as_str() {
            "--dumps" => dumps = Some(PathBuf::from(v)),
            "--threads" => threads = v.parse().expect("--threads"),
            "--reps" => reps = v.parse().expect("--reps"),
            "--out-json" => out_json = Some(PathBuf::from(v)),
            "--out-tex" => out_tex = Some(PathBuf::from(v)),
            _ => panic!("unknown argument {k}"),
        }
    }
    Args {
        dumps: dumps.expect("--dumps"),
        threads,
        reps,
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

/// The kept detections of window `index`, as THOR's transformed detections.
fn read_window(parquet: &Path, index: u32) -> Vec<TransformedDetection> {
    let file = File::open(parquet).expect("open parquet");
    let reader = ParquetRecordBatchReaderBuilder::try_new(file).expect("parquet").build().expect("reader");
    let mut out = Vec::new();
    for batch in reader {
        let b = batch.expect("batch");
        let col = |name: &str| b.column(b.schema().index_of(name).expect(name)).clone();
        let window = col("window");
        let window = window.as_any().downcast_ref::<UInt32Array>().expect("window u32");
        let kept = col("kept");
        let kept = kept.as_any().downcast_ref::<BooleanArray>().expect("kept bool");
        let id_hi = col("id_hi");
        let id_hi = id_hi.as_any().downcast_ref::<UInt64Array>().expect("id_hi");
        let id_lo = col("id_lo");
        let id_lo = id_lo.as_any().downcast_ref::<UInt64Array>().expect("id_lo");
        let state = col("state_id");
        let state = state.as_any().downcast_ref::<UInt64Array>().expect("state_id");
        let night = col("night");
        let night = night.as_any().downcast_ref::<Int64Array>().expect("night");
        let epoch = col("epoch_tdb");
        let epoch = epoch.as_any().downcast_ref::<Float64Array>().expect("epoch_tdb");
        let tx = col("theta_x");
        let tx = tx.as_any().downcast_ref::<Float64Array>().expect("theta_x");
        let ty = col("theta_y");
        let ty = ty.as_any().downcast_ref::<Float64Array>().expect("theta_y");
        for i in 0..b.num_rows() {
            if window.value(i) != index || !kept.value(i) {
                continue;
            }
            out.push(TransformedDetection {
                id: ((id_hi.value(i) as u128) << 64) | id_lo.value(i) as u128,
                state_id: state.value(i),
                night: night.value(i),
                coords: GnomonicCoords {
                    theta_x: tx.value(i),
                    theta_y: ty.value(i),
                    vtheta_x: 0.0,
                    vtheta_y: 0.0,
                    epoch: epoch.value(i),
                    covariance: None,
                    origin: Origin::Sun,
                },
                obs_code: 0,
                dtheta_dr: [0.0; 2],
                d2theta_dr2: [0.0; 2],
                sigma_substitution: Default::default(),
                mag: f64::NAN,
            });
        }
    }
    out
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    if n == 0 {
        f64::NAN
    } else if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    }
}

/// Time one algorithm on one window: wall seconds per repeat, clusters returned, distinct member sets.
fn time_algorithm(
    algorithm: ClusteringAlgorithm,
    min_nights: usize,
    dets: &[TransformedDetection],
    grid: &[(f64, f64)],
    reps: usize,
) -> (Vec<f64>, usize, usize) {
    let mut config = THORConfig::default().clustering;
    config.algorithm = algorithm;
    // The window's own rule, as THOR applies it to windows whose partial groupings are carried: k distinct exposures
    // on m nights, with no arc requirement (the discovery criterion applies to the carried union, not the window).
    if let ClusteringAlgorithm::Hotspot2D { min_count: k, .. } | ClusteringAlgorithm::DBSCAN { min_samples: k, .. } =
        config.algorithm
    {
        config.filter.min_obs = k;
    }
    config.filter.min_nights = min_nights;
    config.filter.min_arc_length = 0.0;
    let backend = CPUClusteringBackend;
    let mut walls = Vec::with_capacity(reps);
    let (mut n_clusters, mut n_distinct) = (0, 0);
    for _ in 0..reps {
        let t = Instant::now();
        let clusters = backend.cluster(dets, grid, &config);
        walls.push(t.elapsed().as_secs_f64());
        n_clusters = clusters.len();
        let sets: HashSet<Vec<usize>> = clusters
            .iter()
            .map(|c| {
                let mut m = c.members.clone();
                m.sort_unstable();
                m
            })
            .collect();
        n_distinct = sets.len();
    }
    (walls, n_clusters, n_distinct)
}

fn main() {
    let args = parse_args();
    let pool = rayon::ThreadPoolBuilder::new().num_threads(args.threads).build().expect("pool");
    let adaptive = AdaptiveClusteringConfig::default();

    let mut jsons: Vec<PathBuf> = fs::read_dir(&args.dumps)
        .expect("dumps dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    jsons.sort();

    let mut inputs = Vec::new();
    let mut rows = Vec::new();
    let (mut speedups, mut cluster_ratios, mut hot_secs, mut db_secs) = (vec![], vec![], vec![], vec![]);
    let mut regions = HashSet::new();
    for json_path in &jsons {
        let parquet = json_path.with_extension("parquet");
        if !parquet.exists() {
            continue;
        }
        let meta: Value = serde_json::from_reader(File::open(json_path).expect("json")).expect("parse json");
        inputs.push(json!({"file": json_path.file_name().unwrap().to_string_lossy(), "sha256": sha256(json_path)}));
        inputs.push(json!({"file": parquet.file_name().unwrap().to_string_lossy(), "sha256": sha256(&parquet)}));
        let window_days = meta["window_days"].as_f64().expect("window_days");
        for w in meta["windows"].as_array().expect("windows") {
            let k = w["rule"]["min_obs"].as_u64().expect("rule") as usize;
            let m = w["rule"]["min_nights"].as_u64().expect("rule") as usize;
            if k < 3 {
                continue; // one-night windows are carried as tracklets, not searched over velocities
            }
            let index = w["index"].as_u64().expect("index") as u32;
            let radius = w["radius_deg"].as_f64().expect("radius_deg");
            let cov: Vec<Vec<f64>> = serde_json::from_value(w["velocity_covariance"].clone()).expect("cov");
            let vel_cov = [[cov[0][0], cov[0][1]], [cov[1][0], cov[1][1]]];
            let step = radius / window_days;
            let grid = generate_elliptical_velocity_grid(&vel_cov, step, &adaptive, ENVELOPE_N_SIGMA);
            let dets = read_window(&parquet, index);
            if dets.is_empty() || grid.is_empty() {
                continue;
            }
            let hot = ClusteringAlgorithm::Hotspot2D { cell_size: radius, min_count: k };
            let db = ClusteringAlgorithm::DBSCAN { eps: radius, min_samples: k };
            let (hot_w, hot_n, hot_d) = pool.install(|| time_algorithm(hot, m, &dets, &grid, args.reps));
            let (db_w, db_n, db_d) = pool.install(|| time_algorithm(db, m, &dets, &grid, args.reps));
            let (h, d) = (median(hot_w.clone()), median(db_w.clone()));
            speedups.push(d / h);
            hot_secs.push(h);
            db_secs.push(d);
            if hot_n > 0 {
                cluster_ratios.push(db_n as f64 / hot_n as f64);
            }
            regions.insert(json_path.file_stem().unwrap().to_string_lossy().to_string());
            eprintln!(
                "{} w{}: {} det, {} hyp, R={:.2}\": hotspot {:.3}s ({} clusters, {} distinct), dbscan {:.3}s ({} clusters, {} distinct)",
                json_path.file_stem().unwrap().to_string_lossy(), index, dets.len(), grid.len(), radius * 3600.0,
                h, hot_n, hot_d, d, db_n, db_d
            );
            rows.push(json!({
                "region": json_path.file_stem().unwrap().to_string_lossy(), "window": index,
                "detections": dets.len(), "hypotheses": grid.len(), "radius_arcsec": radius * 3600.0,
                "window_days": window_days, "rule_min_obs": k, "rule_min_nights": m,
                "hotspot2d": {"wall_s": hot_w, "median_s": h, "clusters": hot_n, "distinct_sets": hot_d},
                "dbscan": {"wall_s": db_w, "median_s": d, "clusters": db_n, "distinct_sets": db_d},
                "speedup": d / h,
            }));
        }
    }

    let cpu = fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("model name")).map(|l| l.split(':').nth(1).unwrap_or("").trim().to_string()))
        .unwrap_or_default();
    let sp = {
        let mut s = speedups.clone();
        s.sort_by(|a, b| a.partial_cmp(b).unwrap());
        s
    };
    let summary = json!({
        "windows": rows.len(), "regions": regions.len(),
        "speedup_median": median(speedups.clone()), "speedup_min": sp.first(), "speedup_max": sp.last(),
        "hotspot2d_median_s": median(hot_secs.clone()), "dbscan_median_s": median(db_secs.clone()),
        "dbscan_to_hotspot2d_clusters_median": median(cluster_ratios.clone()),
    });
    let record = json!({
        "experiment": "clustering_speed",
        "thor_pin": env!("THOR_PIN"), "thor_source": env!("THOR_SOURCE"),
        "threads": args.threads, "reps": args.reps, "cpu": cpu,
        "envelope_n_sigma": ENVELOPE_N_SIGMA, "step": "R / W",
        "inputs": inputs, "summary": summary, "windows": rows,
    });
    if let Some(dir) = args.out_json.parent() {
        fs::create_dir_all(dir).ok();
    }
    fs::write(&args.out_json, serde_json::to_string_pretty(&record).unwrap()).expect("write json");

    // Numbers for the manuscript, as LaTeX macros: never typed by hand.
    let s = &record["summary"];
    let f = |k: &str| s[k].as_f64().unwrap_or(f64::NAN);
    let tex = format!(
        "%% Generated by experiments/src/bin/clustering_speed.rs (THOR {pin}); do not edit.\n\
         \\newcommand{{\\csWindows}}{{{w}}}\n\\newcommand{{\\csRegions}}{{{r}}}\n\\newcommand{{\\csThreads}}{{{t}}}\n\
         \\newcommand{{\\csSpeedup}}{{{m:.1}}}\n\\newcommand{{\\csSpeedupMin}}{{{lo:.1}}}\n\\newcommand{{\\csSpeedupMax}}{{{hi:.1}}}\n\
         \\newcommand{{\\csHotspotSeconds}}{{{hs:.2}}}\n\\newcommand{{\\csDbscanSeconds}}{{{ds:.2}}}\n\
         \\newcommand{{\\csClusterRatio}}{{{cr:.1}}}\n",
        pin = &env!("THOR_PIN")[..8.min(env!("THOR_PIN").len())],
        w = rows.len(), r = regions.len(), t = args.threads,
        m = f("speedup_median"), lo = f("speedup_min"), hi = f("speedup_max"),
        hs = f("hotspot2d_median_s"), ds = f("dbscan_median_s"), cr = f("dbscan_to_hotspot2d_clusters_median"),
    );
    if let Some(dir) = args.out_tex.parent() {
        fs::create_dir_all(dir).ok();
    }
    fs::write(&args.out_tex, tex).expect("write tex");
    eprintln!("summary: {}", serde_json::to_string(&record["summary"]).unwrap());
}
