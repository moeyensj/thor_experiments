//! The design constants Paper A prints, read from THOR at its pin.
//!
//! Every value here comes from THOR itself, in this order of preference: a public constant or function of THOR
//! (`sigma_radius(3, 2)`, `LADDER_RATIO`, `PARTIAL_RULE`, ...); `THORConfig::default()`, serialised; and, for the values
//! the reported searches set, the run config the r07dp2 search ran (`--run-config`, recorded with its SHA-256). No value is
//! typed here: a constant THOR keeps private, or hard-codes inside a function, is left out and named in the record's
//! `not_sourced` list instead.
//!
//! What ran is not only the config file. r07dp2's grid contract rewrote some keys for every test orbit
//! (`GridContract::apply`). So for every job of the run (`--run-dir`, its `jobs.json`) this applies the job's own contract
//! to the config file with THOR's own `apply`, at the pin, and records which keys it changed; what each finished job's
//! run.json `config_snapshot` holds at those keys; the execution policy each run.json states; and which keys each job's
//! own config.json differs from the config file in.
//!
//! Usage:
//!   constants --run-config PATH --run-dir PATH --out-json PATH --out-tex PATH
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use thor::bvh::config::default_guard_corner_factor;
use thor::clustering::adaptive::{AUTO_ORIGIN_EARTH_RHO_MAX, AUTO_ORIGIN_HELIO_R_MIN, BACKGROUND_ASTROMETRIC_FLOOR_MIN};
use thor::clustering::background::{PARTIAL_RULE, PATCH_SIDE_DEG};
use thor::clustering::tracklets::TrackletConfig;
use thor::criterion::Criterion as DiscoveryCriterion;
use thor::orbit_determination::range_grid::RangeGridConfig;
use thor::pipeline::stages::seeds::{AccumulateConfig, SeedCarryForwardConfig};
use thor::pipeline::THORConfig;
use thor::sigma::{dof, sigma_radius, ResolvedSigmas};
use thor::sizing::{sigma_point_offsets, Criterion as SizingCriterion, LADDER_RATIO, TWO_NIGHTS_DAYS, WINDOW_MIN_DAYS};
use thor::test_orbits::config::default_cov_half_width_n_sigma;
use thor::test_orbits::contract::GridContract;
use thor::test_orbits::frame_tiler::{DEFAULT_POSITION_MARGIN, DEFAULT_SIZING_N_SIGMA};

/// The paper commit the `text` fields describe (thor_paper2, branch paper-a-tracklets-carry-od).
const TEXT_AS_OF: &str = "thor_paper2 eee9aa5 (paper-a-tracklets-carry-od)";
/// The grid source named in `ResolvedSigmas::resolve`'s refusals: no grid file is read here; the tiling sigma is THOR's
/// own default.
const GRID_SOURCE: &str = "THOR's default tiling (DEFAULT_SIZING_N_SIGMA)";

struct Args {
    run_config: PathBuf,
    run_dir: PathBuf,
    out_json: PathBuf,
    out_tex: PathBuf,
}

fn parse_args() -> Args {
    let mut a = std::env::args().skip(1);
    let (mut run_config, mut run_dir, mut out_json, mut out_tex) = (None, None, None, None);
    while let Some(k) = a.next() {
        let v = a.next().unwrap_or_else(|| panic!("{k} needs a value"));
        match k.as_str() {
            "--run-config" => run_config = Some(PathBuf::from(v)),
            "--run-dir" => run_dir = Some(PathBuf::from(v)),
            "--out-json" => out_json = Some(PathBuf::from(v)),
            "--out-tex" => out_tex = Some(PathBuf::from(v)),
            _ => panic!("unknown argument {k}"),
        }
    }
    Args {
        run_config: run_config.expect("--run-config"),
        run_dir: run_dir.expect("--run-dir"),
        out_json: out_json.expect("--out-json"),
        out_tex: out_tex.expect("--out-tex"),
    }
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

fn read_json(path: &Path) -> Value {
    serde_json::from_reader(File::open(path).unwrap_or_else(|e| panic!("open {}: {e}", path.display())))
        .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

// ── Reading a serialised config ───────────────────────────────────────────────────────────────────────────────────

/// The value at a JSON pointer, refusing a pointer the config does not have: a key that moved must not read as absent.
fn at<'a>(v: &'a Value, ptr: &str) -> &'a Value {
    v.pointer(ptr).unwrap_or_else(|| panic!("no {ptr} in the serialised config"))
}

fn num(v: &Value, ptr: &str) -> f64 {
    at(v, ptr).as_f64().unwrap_or_else(|| panic!("{ptr} is not a number: {}", at(v, ptr)))
}

/// A config key as a dotted name, for the record: `/dc/huber_k` -> `dc.huber_k`.
fn key(ptr: &str) -> String {
    ptr.trim_start_matches('/').replace('/', ".")
}

/// Where two trees differ, as `pointer -> (a, b)`: objects are compared key by key (`null` where a side lacks the key);
/// anything else, including a key whose value changes from a scalar to an object, is one difference at its pointer.
fn diff(a: &Value, b: &Value) -> BTreeMap<String, (Value, Value)> {
    fn rec(a: &Value, b: &Value, ptr: String, out: &mut BTreeMap<String, (Value, Value)>) {
        match (a, b) {
            (Value::Object(ma), Value::Object(mb)) => {
                for k in ma.keys().chain(mb.keys().filter(|k| !ma.contains_key(*k))) {
                    let (x, y) = (ma.get(k).unwrap_or(&Value::Null), mb.get(k).unwrap_or(&Value::Null));
                    rec(x, y, format!("{ptr}/{k}"), out);
                }
            }
            _ if a != b => {
                out.insert(ptr, (a.clone(), b.clone()));
            }
            _ => {}
        }
    }
    let mut out = BTreeMap::new();
    rec(a, b, String::new(), &mut out);
    out
}

/// Remove a dotted key from a JSON tree (the fields the config file's `.removed.txt` names).
fn remove_dotted(v: &mut Value, dotted: &str) {
    let parts: Vec<&str> = dotted.split('.').collect();
    let mut cur = v;
    for p in &parts[..parts.len() - 1] {
        match cur.get_mut(*p) {
            Some(x) => cur = x,
            None => return,
        }
    }
    if let Some(m) = cur.as_object_mut() {
        m.remove(*parts.last().unwrap());
    }
}

// ── Formatting a value the way the text prints it ────────────────────────────────────────────────────────────────

/// The shortest decimal that reads back as `x` (Rust's `Display`: 3.0 -> "3", 0.8 -> "0.8").
fn plain(x: f64) -> String {
    format!("{x}")
}

fn fixed(x: f64, places: usize) -> String {
    format!("{x:.places$}")
}

/// An integer-valued float as an integer, refusing one that is not.
fn int(x: f64) -> u64 {
    let r = x.round();
    assert!((x - r).abs() < 1e-9 && r >= 0.0, "{x} is not a whole number");
    r as u64
}

/// Thousands separated the way the other generated files print them: 262{,}144.
fn thousands(n: u64) -> String {
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

fn word(n: u64) -> String {
    const W: [&str; 21] = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve",
        "thirteen", "fourteen", "fifteen", "sixteen", "seventeen", "eighteen", "nineteen", "twenty",
    ];
    W.get(n as usize).unwrap_or_else(|| panic!("no word for {n}")).to_string()
}

fn ordinal(n: u64) -> String {
    const W: [&str; 13] = [
        "zeroth", "first", "second", "third", "fourth", "fifth", "sixth", "seventh", "eighth", "ninth", "tenth",
        "eleventh", "twelfth",
    ];
    W.get(n as usize).unwrap_or_else(|| panic!("no ordinal for {n}")).to_string()
}

fn capitalised(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// A ratio as the smallest fraction p/q (q <= 12) that equals it, refusing one that has none.
fn fraction(x: f64) -> String {
    for q in 1..=12u64 {
        let p = (x * q as f64).round();
        if (x * q as f64 - p).abs() < 1e-9 {
            return format!("{}/{q}", p as u64);
        }
    }
    panic!("{x} is not a small fraction")
}

/// A multiple the text prints in words ("a quarter of", "half again").
fn multiple_in_words(x: f64) -> String {
    match fraction(x).as_str() {
        "1/4" => "a quarter".into(),
        "1/3" => "a third".into(),
        "1/2" => "half".into(),
        "3/2" => "half again".into(),
        f => panic!("no words for the multiple {f}"),
    }
}

// ── The record ─────────────────────────────────────────────────────────────────────────────────────────────────────

struct Macro {
    name: &'static str,
    tex: String,
    value: Value,
    kind: &'static str,
    source: String,
    /// Where the text prints it (as of `TEXT_AS_OF`); empty where it does not.
    text: &'static str,
    /// Other places THOR states the same value, and what each says.
    checks: Vec<Value>,
    note: Option<String>,
}

#[derive(Default)]
struct Book {
    macros: Vec<Macro>,
    /// Places where two of THOR's own statements of one value disagree.
    disagreements: Vec<Value>,
}

impl Book {
    #[allow(clippy::too_many_arguments)]
    fn add(
        &mut self,
        name: &'static str,
        tex: String,
        value: Value,
        kind: &'static str,
        source: impl Into<String>,
        text: &'static str,
        checks: Vec<Value>,
        note: Option<&str>,
    ) {
        assert!(name.starts_with("cn"), "{name}: every macro here is \\cn...");
        assert!(!self.macros.iter().any(|m| m.name == name), "{name} twice");
        for c in &checks {
            if c["agrees"] == json!(false) {
                self.disagreements.push(json!({"macro": name, "value": value, "other": c}));
            }
        }
        self.macros.push(Macro {
            name,
            tex,
            value,
            kind,
            source: source.into(),
            text,
            checks,
            note: note.map(str::to_string),
        });
    }
}

/// One other statement of a value, and whether it agrees with the macro's.
fn check(source: impl Into<String>, value: Value, primary: &Value) -> Value {
    let agrees = match (value.as_f64(), primary.as_f64()) {
        (Some(a), Some(b)) => (a - b).abs() <= 1e-12 * b.abs().max(1.0),
        _ => &value == primary,
    };
    json!({"source": source.into(), "value": value, "agrees": agrees})
}

/// The value at `ptr` in THORConfig::default() and in the run config file, as checks.
fn config_checks(ptr: &str, def: &Value, run: &Value, primary: &Value) -> Vec<Value> {
    vec![
        check(format!("THORConfig::default() {}", key(ptr)), at(def, ptr).clone(), primary),
        check(format!("run config {}", key(ptr)), at(run, ptr).clone(), primary),
    ]
}

/// A count of nodes on one axis of the range grid: 2m + 1, with m the grid's own half-node count.
fn nodes(half: i64) -> u64 {
    assert!(half >= 0);
    (2 * half + 1) as u64
}

/// The execution policy run.json records for each of a job's shells.
fn policy_tuple(mp: &Value) -> Value {
    json!({
        "search_mode": mp["search_mode"],
        "tracklets": mp["tracklets"],
        "window_mode": mp["window_mode"]["mode"],
        "carry_hypotheses": mp["window_mode"]["carry_hypotheses"],
        "arms_cross_night_chains": mp["arms_cross_night_chains"],
    })
}

fn tally(counts: &mut Vec<(Value, usize)>, v: Value) {
    match counts.iter_mut().find(|(k, _)| *k == v) {
        Some((_, n)) => *n += 1,
        None => counts.push((v, 1)),
    }
}

fn tally_json(counts: Vec<(Value, usize)>) -> Value {
    Value::Array(counts.into_iter().map(|(k, n)| json!({"value": k, "count": n})).collect())
}

fn main() {
    let args = parse_args();
    let mut inputs: Vec<Value> = Vec::new();

    // ── THOR's default config, and the run config file at the pin ──────────────────────────────────────────────────
    let default_cfg = THORConfig::default();
    let def = serde_json::to_value(&default_cfg).expect("serialise THORConfig::default()");
    let run = read_json(&args.run_config);
    inputs.push(json!({"file": args.run_config.display().to_string(), "role": "run config (r07dp2)",
                       "sha256": sha256(&args.run_config)}));
    let run_cfg: THORConfig = serde_json::from_value(run.clone())
        .unwrap_or_else(|e| panic!("{} does not load as a THORConfig at the pin: {e}", args.run_config.display()));
    let removed_path = PathBuf::from(format!("{}.removed.txt", args.run_config.display()));
    let removed: Vec<String> = if removed_path.exists() {
        inputs.push(json!({"file": removed_path.display().to_string(),
                           "role": "fields removed from the job config to make the run config file",
                           "sha256": sha256(&removed_path)}));
        fs::read_to_string(&removed_path).expect("removed.txt").lines().map(str::trim).filter(|l| !l.is_empty())
            .map(str::to_string).collect()
    } else {
        Vec::new()
    };

    // ── What r07dp2 ran: each job's contract applied to the config file, by THOR's own apply, at the pin ───────────
    // The THOR the run itself ran: the run directory's image records that name this run.
    let run_name = args.run_dir.file_name().expect("run dir name").to_string_lossy().to_string();
    let mut images = Vec::new();
    let mut entries: Vec<PathBuf> = fs::read_dir(&args.run_dir)
        .expect("run dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            let name = p.file_name().unwrap().to_string_lossy();
            name.starts_with("image_") && p.extension().is_some_and(|x| x == "json")
        })
        .collect();
    entries.sort();
    for p in entries {
        let img = read_json(&p);
        if img["used_by"].as_array().is_some_and(|u| u.iter().any(|x| x.as_str() == Some(run_name.as_str()))) {
            inputs.push(json!({"file": p.display().to_string(), "role": "the THOR image the run ran", "sha256": sha256(&p)}));
            images.push(json!({"tag": img["tag"], "digest": img["digest"]}));
        }
    }
    assert!(!images.is_empty(), "no image record in {} names {run_name}", args.run_dir.display());

    let jobs_path = args.run_dir.join("jobs.json");
    inputs.push(json!({"file": jobs_path.display().to_string(), "role": "r07dp2 jobs", "sha256": sha256(&jobs_path)}));
    let jobs = read_json(&jobs_path);
    let mut job_records = Vec::new();
    let mut applied_configs: Vec<Value> = Vec::new();
    let mut override_counts: Vec<(Value, usize)> = Vec::new();
    let mut policy_counts: Vec<(Value, usize)> = Vec::new();
    let mut job_config_diff_counts: Vec<(Value, usize)> = Vec::new();
    let mut snapshot_counts: Vec<(Value, usize)> = Vec::new();
    let (mut n_jobs, mut n_finished) = (0usize, 0usize);
    let before = serde_json::to_value(&run_cfg).expect("serialise run config");
    for job in jobs.as_array().expect("jobs.json is a list") {
        n_jobs += 1;
        let id = job["test_orbit"].as_str().expect("job test_orbit");
        let contract_path = PathBuf::from(job["contract"].as_str().expect("job contract"));
        let contract = GridContract::load(&contract_path)
            .unwrap_or_else(|e| panic!("{id}: contract {} does not load at the pin: {e}", contract_path.display()));
        let mut applied = run_cfg.clone();
        let shell = contract.apply(&mut applied, id);
        assert!(shell.is_some(), "{id}: its contract {} has no shell for it", contract_path.display());
        let after = serde_json::to_value(&applied).expect("serialise applied config");
        let changed = diff(&before, &after);
        for (ptr, (from, to)) in &changed {
            tally(&mut override_counts, json!({"key": key(ptr), "from": from, "to": to}));
        }

        let mut rec = Map::new();
        rec.insert("test_orbit".into(), json!(id));
        rec.insert("contract".into(), json!(contract_path.display().to_string()));
        rec.insert("contract_sha256".into(), json!(sha256(&contract_path)));
        let results = args.run_dir.join("results").join(id);
        let config_path = results.join("config.json");
        if config_path.exists() {
            rec.insert("config_sha256".into(), json!(sha256(&config_path)));
            let mut job_cfg = read_json(&config_path);
            for r in &removed {
                remove_dotted(&mut job_cfg, r);
            }
            let keys: Vec<String> = diff(&run, &job_cfg).keys().map(|p| key(p)).collect();
            tally(&mut job_config_diff_counts, json!(keys));
        }
        let run_json_path = results.join("run.json");
        if run_json_path.exists() {
            n_finished += 1;
            rec.insert("run_json_sha256".into(), json!(sha256(&run_json_path)));
            let rj = read_json(&run_json_path);
            for mp in rj["grid_policy"]["measurement_policies"].as_array().expect("measurement_policies") {
                tally(&mut policy_counts, policy_tuple(mp));
            }
            // What run.json's config_snapshot holds at each key the contract changed: the job's own config before the
            // contract, the contract's value, or something else.
            let job_cfg = if config_path.exists() { read_json(&config_path) } else { Value::Null };
            for (ptr, (_, to)) in &changed {
                let snap = rj["config_snapshot"].pointer(ptr).unwrap_or(&Value::Null);
                let own = job_cfg.pointer(ptr).unwrap_or(&Value::Null);
                let class = if snap == to {
                    "the contract's value"
                } else if snap == own {
                    "the job's own config.json value (before the contract)"
                } else {
                    "neither"
                };
                tally(&mut snapshot_counts, json!({"key": key(ptr), "snapshot_holds": class}));
            }
        }
        job_records.push(Value::Object(rec));
        applied_configs.push(after);
    }
    // The value at `ptr` over every job's applied config.
    let ran = |ptr: &str| -> Value {
        let mut counts = Vec::new();
        for c in &applied_configs {
            tally(&mut counts, at(c, ptr).clone());
        }
        tally_json(counts)
    };
    let ran_check = |ptr: &str, primary: &Value| -> Value {
        let t = ran(ptr);
        let all_agree = t.as_array().unwrap().iter().all(|e| check("", e["value"].clone(), primary)["agrees"] == json!(true));
        json!({"source": format!("r07dp2 as run: {} after each job's GridContract::apply", key(ptr)), "value": t,
               "agrees": all_agree})
    };

    let mut book = Book::default();

    // ── The test orbit and its volume (Section 2.2) ────────────────────────────────────────────────────────────────
    let n = DEFAULT_SIZING_N_SIGMA;
    let resolved_default = ResolvedSigmas::resolve(&default_cfg, n, GRID_SOURCE).expect("resolve the default config");
    let resolved_run = ResolvedSigmas::resolve(&run_cfg, n, GRID_SOURCE).expect("resolve the run config");
    let nv = json!(n);
    book.add("cnTilingNSigma", plain(n), nv.clone(), "public const",
        "thor::test_orbits::frame_tiler::DEFAULT_SIZING_N_SIGMA",
        "Sec. 2.2 'divided by $n = 3$'; Sec. 3.1 'rather than 3', 'a 3$\\sigma$ box'; Sec. 7 '$n = 3$ standard deviations'",
        vec![
            check("thor::test_orbits::config::default_cov_half_width_n_sigma()", json!(default_cov_half_width_n_sigma()), &nv),
            check("ResolvedSigmas::resolve(THORConfig::default(), n).velocity_envelope_n_sigma (the velocity ellipse)",
                  json!(resolved_default.velocity_envelope_n_sigma), &nv),
            check("ResolvedSigmas::resolve(run config, n).velocity_envelope_n_sigma",
                  json!(resolved_run.velocity_envelope_n_sigma), &nv),
        ],
        Some("the velocity ellipse's n (Sec. 4.1, Sec. 7) is adaptive.velocity_envelope_n_sigma = null, which resolves to \
              the grid's tiling sigma"));
    let pm = DEFAULT_POSITION_MARGIN;
    book.add("cnPixelMargin", plain(pm), json!(pm), "public const",
        "thor::test_orbits::frame_tiler::DEFAULT_POSITION_MARGIN", "", vec![], None);
    book.add("cnPixelMarginWords", multiple_in_words(pm), json!(pm), "public const",
        "thor::test_orbits::frame_tiler::DEFAULT_POSITION_MARGIN", "Sec. 2.2 'half again the pixel's half-width'",
        vec![], None);

    // ── The discovery criterion (Section 2.3) ──────────────────────────────────────────────────────────────────────
    let crit = DiscoveryCriterion::default();
    let co = json!(crit.min_obs);
    let cn = json!(crit.min_nights);
    let ca = json!(crit.min_arc_days);
    let crit_checks = |ptr_obs: &str, ptr_nights: &str, ptr_arc: &str| -> [Vec<Value>; 3] {
        [
            config_checks(ptr_obs, &def, &run, &co),
            config_checks(ptr_nights, &def, &run, &cn),
            config_checks(ptr_arc, &def, &run, &ca),
        ]
    };
    let [mut obs_checks, mut nights_checks, mut arc_checks] =
        crit_checks("/clustering/filter/min_obs", "/clustering/filter/min_nights", "/clustering/filter/min_arc_length");
    let [o2, n2, a2] = crit_checks("/od_filter/min_obs", "/od_filter/min_nights", "/od_filter/min_arc_length");
    obs_checks.extend(o2);
    nights_checks.extend(n2);
    arc_checks.extend(a2);
    obs_checks.extend(config_checks("/clustering/algorithm/Hotspot2D/min_count", &def, &run, &co));
    book.add("cnCriterionMinObs", crit.min_obs.to_string(), co.clone(), "public fn",
        "thor::criterion::Criterion::default().min_obs", "Sec. 4.1 '$k = 6$ detections'", obs_checks, None);
    book.add("cnCriterionMinObsWord", word(crit.min_obs as u64), co.clone(), "public fn",
        "thor::criterion::Criterion::default().min_obs", "Sec. 2.3 'at least six of its detections'; Sec. 5.1 'for six on four'",
        vec![], None);
    book.add("cnCriterionMinNights", crit.min_nights.to_string(), cn.clone(), "public fn",
        "thor::criterion::Criterion::default().min_nights", "Sec. 4.1 '$m = 4$ nights'", nights_checks, None);
    book.add("cnCriterionMinNightsWord", word(crit.min_nights as u64), cn.clone(), "public fn",
        "thor::criterion::Criterion::default().min_nights", "Sec. 2.3 'on at least four nights'; Sec. 5.1 'for six on four'",
        vec![], None);
    book.add("cnCriterionMinArcDays", plain(crit.min_arc_days), ca.clone(), "public fn",
        "thor::criterion::Criterion::default().min_arc_days", "Sec. 2.3 'span at least 2.7 days'", arc_checks, None);
    let floor = SizingCriterion { min_obs: crit.min_obs, min_nights: crit.min_nights, min_arc_days: crit.min_arc_days }
        .window_floor_days();
    book.add("cnWindowFloorDays", int(floor).to_string(), json!(floor), "public fn",
        "thor::sizing::Criterion::window_floor_days() at thor::criterion::Criterion::default()", "", vec![],
        Some("max(min_arc_days, min_nights - 1), as the text says"));
    book.add("cnWindowFloorDaysWord", word(int(floor)), json!(floor), "public fn",
        "thor::sizing::Criterion::window_floor_days() at thor::criterion::Criterion::default()",
        "Sec. 2.3 'is three days'; Sec. 5.2 '(three days; ...)'", vec![], None);

    let po = json!(PARTIAL_RULE.min_obs);
    let pn = json!(PARTIAL_RULE.min_nights);
    let carry_default = serde_json::to_value(SeedCarryForwardConfig::default()).expect("serialise");
    let carry_note = "The carry's floor as run is PARTIAL_RULE: contract::enable_own_fit_carry sets \
                      seed_carryforward.min_obs/min_nights to it on every lattice shell, over the run config's 5 on 3 \
                      (and over THORConfig::default()'s 5 on 3).";
    let mut po_checks = vec![
        check("SeedCarryForwardConfig::default().min_obs", carry_default["min_obs"].clone(), &po),
        check("THORConfig::default() seed_carryforward.min_obs", at(&def, "/seed_carryforward/min_obs").clone(), &po),
        check("run config seed_carryforward.min_obs", at(&run, "/seed_carryforward/min_obs").clone(), &po),
    ];
    po_checks.push(ran_check("/seed_carryforward/min_obs", &po));
    let mut pn_checks = vec![
        check("SeedCarryForwardConfig::default().min_nights", carry_default["min_nights"].clone(), &pn),
        check("THORConfig::default() seed_carryforward.min_nights", at(&def, "/seed_carryforward/min_nights").clone(), &pn),
        check("run config seed_carryforward.min_nights", at(&run, "/seed_carryforward/min_nights").clone(), &pn),
    ];
    pn_checks.push(ran_check("/seed_carryforward/min_nights", &pn));
    book.add("cnPartialMinObs", PARTIAL_RULE.min_obs.to_string(), po.clone(), "public const",
        "thor::clustering::background::PARTIAL_RULE.min_obs", "Sec. 4.1 '$k = 4$'", po_checks, Some(carry_note));
    book.add("cnPartialMinObsWord", word(PARTIAL_RULE.min_obs as u64), po.clone(), "public const",
        "thor::clustering::background::PARTIAL_RULE.min_obs",
        "Sec. 2.3, 4.3, 5.1 (twice, and Fig. 3's caption) 'four detections on two nights'; Sec. 5.1 'a chance grouping of four'",
        vec![], None);
    book.add("cnPartialMinNights", PARTIAL_RULE.min_nights.to_string(), pn.clone(), "public const",
        "thor::clustering::background::PARTIAL_RULE.min_nights", "Sec. 4.1 '$m = 2$'", pn_checks, Some(carry_note));
    book.add("cnPartialMinNightsWord", word(PARTIAL_RULE.min_nights as u64), pn.clone(), "public const",
        "thor::clustering::background::PARTIAL_RULE.min_nights",
        "Sec. 2.3, 4.3, 5.1 (twice, and Fig. 3's caption) 'on two nights'; Sec. 5.1 'two such pairs on two nights'",
        vec![], None);

    // ── The gather (Section 3.1) ───────────────────────────────────────────────────────────────────────────────────
    let corner = default_guard_corner_factor();
    let cv = json!(corner);
    let corner_checks = config_checks("/bvh/query/guard/CovarianceOriented/corner_factor", &def, &run, &cv);
    book.add("cnGuardCornerFactor", fraction(corner), cv, "public fn",
        "thor::bvh::config::default_guard_corner_factor()", "", corner_checks, None);
    let guard = resolved_default.gather_guard_n_sigma.expect("the default guard is covariance-oriented");
    let gv = json!(guard);
    book.add("cnGuardNSigma", int(guard).to_string(), gv.clone(), "public fn",
        "ResolvedSigmas::resolve(THORConfig::default(), DEFAULT_SIZING_N_SIGMA).gather_guard_n_sigma \
         (bvh.query.guard: the tiling sigma times the corner factor)",
        "Sec. 3.1 'drawn at 5 standard deviations'",
        vec![check("ResolvedSigmas::resolve(run config, n).gather_guard_n_sigma",
                   json!(resolved_run.gather_guard_n_sigma), &gv)],
        Some("\\fgGuardNSigma (fig_gather) is the same guard as that figure's run resolved it"));
    // A per-axis box of half-width n spanning d whitened directions has its corner at n sqrt(d) (bvh::config's
    // default_guard_corner_factor doc: short-arc sky position spans 2 to 3 of them).
    let c2 = n * 2f64.sqrt();
    let c3 = n * 3f64.sqrt();
    book.add("cnBoxCornerTwoDirections", fixed(c2, 1), json!(c2), "derived",
        "DEFAULT_SIZING_N_SIGMA * sqrt(2)", "Sec. 3.1 'corners at 4.2'", vec![], None);
    book.add("cnBoxCornerThreeDirections", fixed(c3, 1), json!(c3), "derived",
        "DEFAULT_SIZING_N_SIGMA * sqrt(3)", "Sec. 3.1 'to 5.2$\\sigma$'", vec![], None);
    let chord = num(&def, "/bvh/index/chord_sigma_fraction");
    let chv = json!(chord);
    book.add("cnChordFraction", plain(chord), chv.clone(), "config default", "bvh.index.chord_sigma_fraction", "",
        vec![check("run config bvh.index.chord_sigma_fraction", at(&run, "/bvh/index/chord_sigma_fraction").clone(), &chv)],
        None);
    book.add("cnChordFractionWords", multiple_in_words(chord), chv, "config default", "bvh.index.chord_sigma_fraction",
        "Sec. 3.1 'by more than a quarter of the ellipse's size'", vec![], None);

    // ── The frame (Section 3.2) ────────────────────────────────────────────────────────────────────────────────────
    let origin_note = format!(
        "the rule applies under tangent_origin = Auto (THORConfig::default(): {}; run config: {}); r07dp2's contracts, \
         applied at the pin, set tangent_origin to {}",
        at(&def, "/tangent_origin"),
        at(&run, "/tangent_origin"),
        ran("/tangent_origin")
    );
    book.add("cnOriginEarthRhoMax", plain(AUTO_ORIGIN_EARTH_RHO_MAX), json!(AUTO_ORIGIN_EARTH_RHO_MAX), "public const",
        "thor::clustering::adaptive::AUTO_ORIGIN_EARTH_RHO_MAX (au, mean geocentric distance below which the frame is \
         geocentric)", "", vec![], Some(origin_note.as_str()));
    book.add("cnOriginHelioRMin", plain(AUTO_ORIGIN_HELIO_R_MIN), json!(AUTO_ORIGIN_HELIO_R_MIN), "public const",
        "thor::clustering::adaptive::AUTO_ORIGIN_HELIO_R_MIN (au, mean heliocentric distance above which the frame is \
         heliocentric)", "", vec![], None);

    // ── The velocity search (Section 4.1) ──────────────────────────────────────────────────────────────────────────
    let cull = num(&def, "/clustering/filter/rchi2_threshold");
    let cullv = json!(cull);
    book.add("cnClusterRChiSq", plain(cull), cullv.clone(), "config default", "clustering.filter.rchi2_threshold",
        "Sec. 4.1 'exceeds 10 is dropped'",
        vec![check("run config clustering.filter.rchi2_threshold", at(&run, "/clustering/filter/rchi2_threshold").clone(), &cullv)],
        None);

    // ── Tracklets and the radius kappa (Sections 4.2, 4.3, 4.4) ────────────────────────────────────────────────────
    let pair_n = num(&def, "/tracklets/pair_n_sigma");
    let pnv = json!(pair_n);
    let mut kn_checks = config_checks("/tracklets/pair_n_sigma", &def, &run, &pnv);
    kn_checks.extend(config_checks("/tracklets/chain_n_sigma", &def, &run, &pnv));
    kn_checks.extend(config_checks("/dc/outlier_n_sigma", &def, &run, &pnv));
    book.add("cnKappaNSigma", plain(pair_n), pnv.clone(), "config default", "tracklets.pair_n_sigma",
        "Sec. 4.2 'the radius holding 3$\\sigma$ in two dimensions'", kn_checks,
        Some("the text's one kappa is three knobs in THOR: the pair gate (tracklets.pair_n_sigma), the claim disc \
              (tracklets.chain_n_sigma) and the OD rejection (dc.outlier_n_sigma); the checks state each"));
    let kappa = resolved_default.pair_radius;
    let kv = json!(kappa);
    let outlier_r = sigma_radius(num(&def, "/dc/outlier_n_sigma"), dof::SKY_RESIDUAL);
    let kappa_checks = vec![
        check("sigma_radius(tracklets.pair_n_sigma, dof::CHART_2D)", json!(sigma_radius(pair_n, dof::CHART_2D)), &kv),
        check("ResolvedSigmas chain_radius (the claim disc, THORConfig::default())", json!(resolved_default.chain_radius), &kv),
        check("sigma_radius(dc.outlier_n_sigma, dof::SKY_RESIDUAL) (the OD rejection)", json!(outlier_r), &kv),
        check("ResolvedSigmas pair_radius (run config)", json!(resolved_run.pair_radius), &kv),
        check("ResolvedSigmas chain_radius (run config)", json!(resolved_run.chain_radius), &kv),
        check("sigma_radius(run dc.outlier_n_sigma, dof::SKY_RESIDUAL)",
              json!(sigma_radius(num(&run, "/dc/outlier_n_sigma"), dof::SKY_RESIDUAL)), &kv),
    ];
    book.add("cnKappa", fixed(kappa, 2), kv.clone(), "public fn",
        "ResolvedSigmas::resolve(THORConfig::default(), n).pair_radius = sigma_radius(tracklets.pair_n_sigma, 2)",
        "Sec. 4.2 '$\\kappa = 3.44$'", kappa_checks, None);
    let kappa_sq = outlier_r * outlier_r;
    book.add("cnKappaSq", fixed(kappa_sq, 1), json!(kappa_sq), "public fn",
        "sigma_radius(dc.outlier_n_sigma, dof::SKY_RESIDUAL)^2 (thor_dc::refine_single's rejection bar)",
        "Sec. 4.4 '$\\kappa^{2} = 11.8$'", vec![], None);
    let sag = at(&def, "/tracklets/sagitta_n_sigma");
    let sag_n = if sag.is_null() { pair_n } else { sag.as_f64().expect("sagitta_n_sigma") };
    book.add("cnSagittaNSigmaWord", word(int(sag_n)), json!(sag_n), "config default",
        "tracklets.sagitta_n_sigma (null: follows tracklets.pair_n_sigma)",
        "Sec. 4.2 'than three times its own uncertainty'",
        vec![check("run config tracklets.sagitta_n_sigma", at(&run, "/tracklets/sagitta_n_sigma").clone(), sag)], None);
    let cad = num(&def, "/grid_sweep_mode/CadenceAdaptive/min_exposures");
    let cadv = json!(cad);
    book.add("cnCadenceMinExposuresWord", word(int(cad)), cadv.clone(), "config default",
        "grid_sweep_mode.CadenceAdaptive.min_exposures", "Sec. 4.2 'imaged at least four times that night'",
        config_checks("/grid_sweep_mode/CadenceAdaptive/min_exposures", &def, &run, &cadv)[1..].to_vec(), None);
    let kval = num(&def, "/weighting/trails/k_valid");
    let kvalv = json!(kval);
    book.add("cnTrailMinLengthSeeing", plain(kval), kvalv.clone(), "config default", "weighting.trails.k_valid",
        "Sec. 4.2 prints 'about twice the seeing'",
        config_checks("/weighting/trails/k_valid", &def, &run, &kvalv)[1..].to_vec(),
        Some("the minimum trail length that measures a rate, in seeings"));

    // ── The carry (Section 4.3) ────────────────────────────────────────────────────────────────────────────────────
    let cap = num(&def, "/tracklets/chain_max_claim_radius_deg");
    let capv = json!(cap);
    book.add("cnClaimCapDeg", plain(cap), capv.clone(), "config default", "tracklets.chain_max_claim_radius_deg",
        "Sec. 4.3 'or $0.05^{\\circ}$'; Sec. 5.3 'a fixed cap of $0.05^{\\circ}$'",
        config_checks("/tracklets/chain_max_claim_radius_deg", &def, &run, &capv)[1..].to_vec(), None);
    let passes = serde_json::to_value(AccumulateConfig::default()).expect("serialise")["max_passes"].clone();
    book.add("cnCarryMaxPasses", passes.to_string(), passes.clone(), "public fn",
        "AccumulateConfig::default().max_passes (seed_carryforward.accumulate.max_passes)",
        "Sec. 4.3 'at most 64 times'",
        vec![
            check("run config seed_carryforward.accumulate", at(&run, "/seed_carryforward/accumulate").clone(), &passes),
            ran_check("/seed_carryforward/accumulate/max_passes", &passes),
        ],
        Some("the run config sets no accumulate block; contract::enable_own_fit_carry installs AccumulateConfig::default() \
              (carry_hypotheses on) on every lattice shell"));
    let store_type = serde_json::to_value(TrackletConfig::default()).expect("serialise")["max_chain_seeds"].clone();
    let store_def = at(&def, "/tracklets/max_chain_seeds").clone();
    // Printed as the text prints it, "4096", without a separator.
    book.add("cnSeedStoreTypeDefault", store_type.as_u64().expect("max_chain_seeds").to_string(), store_type.clone(),
        "public fn", "TrackletConfig::default().max_chain_seeds", "Sec. 4.3 'a store of 4096'",
        vec![
            check("THORConfig::default() tracklets.max_chain_seeds", store_def.clone(), &store_type),
            check("run config tracklets.max_chain_seeds", at(&run, "/tracklets/max_chain_seeds").clone(), &store_type),
            ran_check("/tracklets/max_chain_seeds", &store_type),
        ],
        Some("the sub-config's own default; THORConfig::default() and r07dp2 set 262,144"));
    book.add("cnSeedStoreRun", thousands(at(&run, "/tracklets/max_chain_seeds").as_u64().expect("max_chain_seeds")),
        at(&run, "/tracklets/max_chain_seeds").clone(), "run config", "tracklets.max_chain_seeds", "",
        vec![check("THORConfig::default() tracklets.max_chain_seeds", store_def, at(&run, "/tracklets/max_chain_seeds")),
             ran_check("/tracklets/max_chain_seeds", at(&run, "/tracklets/max_chain_seeds"))],
        None);

    // ── Orbit determination (Section 4.4) ──────────────────────────────────────────────────────────────────────────
    let grid_default: RangeGridConfig =
        serde_json::from_value(at(&def, "/od/range_grid").clone()).expect("THORConfig::default() od.range_grid");
    let grid_run: RangeGridConfig = serde_json::from_value(at(&run, "/od/range_grid").clone()).expect("run od.range_grid");
    let grid_type = RangeGridConfig::default();
    // The range axis: with the range-rate axis following the covariance (`rdot_half_width_au_day = None`),
    // `rdot_half_nodes()` is the range axis's own half-node count (`axis_half_nodes`, private), m = floor(radius / step).
    let range_axis = RangeGridConfig { rdot_half_width_au_day: None, ..grid_default.clone() };
    let m = range_axis.rdot_half_nodes();
    let range_nodes = nodes(m);
    let rnv = json!(range_nodes);
    let range_axis_run = RangeGridConfig { rdot_half_width_au_day: None, ..grid_run.clone() };
    book.add("cnRangeNodes", range_nodes.to_string(), rnv.clone(), "public fn",
        "2 m + 1, m = RangeGridConfig::rdot_half_nodes() with the rate axis on the covariance slice (the range axis's \
         floor(mahalanobis_radius() / step_sigma)), at THORConfig::default() od.range_grid",
        "Sec. 4.4 '13 distances'",
        vec![check("the same at the run config's od.range_grid", json!(nodes(range_axis_run.rdot_half_nodes())), &rnv)],
        Some(format!("the outer nodes sit at +-{} sigma; the disc they are tested against has radius \
                      mahalanobis_radius() = {:.4}", m as f64 * grid_default.step_sigma, grid_default.mahalanobis_radius())
            .as_str()));
    let rns = json!(grid_default.n_sigma);
    book.add("cnRangeNSigma", plain(grid_default.n_sigma), rns.clone(), "config default", "od.range_grid.n_sigma",
        "Sec. 4.4 'across its 3$\\sigma$ extent'", config_checks("/od/range_grid/n_sigma", &def, &run, &rns)[1..].to_vec(),
        None);
    let rss = json!(grid_default.step_sigma);
    book.add("cnRangeStepSigma", plain(grid_default.step_sigma), rss.clone(), "config default", "od.range_grid.step_sigma",
        "", config_checks("/od/range_grid/step_sigma", &def, &run, &rss)[1..].to_vec(), None);
    let hw = grid_default.rdot_half_width_au_day.expect("the default range-rate axis is absolute");
    let hwv = json!(hw);
    book.add("cnRangeRateHalfWidth", plain(hw), hwv.clone(), "config default", "od.range_grid.rdot_half_width_au_day",
        "Sec. 4.4 '$\\pm 0.02$\\,au\\,day$^{-1}$'",
        config_checks("/od/range_grid/rdot_half_width_au_day", &def, &run, &hwv)[1..].to_vec(), None);
    let rr_type = nodes(grid_type.rdot_half_nodes());
    let rr_def = nodes(grid_default.rdot_half_nodes());
    let rr_run = nodes(grid_run.rdot_half_nodes());
    let rrt = json!(rr_type);
    book.add("cnRangeRateNodesTypeDefaultWord", word(rr_type), rrt.clone(), "public fn",
        "2 RangeGridConfig::default().rdot_half_nodes() + 1", "Sec. 4.4 '(seven by default'",
        vec![check("2 rdot_half_nodes() + 1 at THORConfig::default() od.range_grid", json!(rr_def), &rrt),
             check("2 rdot_half_nodes() + 1 at the run config", json!(rr_run), &rrt)],
        Some("RangeGridConfig's own default; THORConfig::default() sets rdot_nodes 27"));
    let rrr = json!(rr_run);
    book.add("cnRangeRateNodesRun", rr_run.to_string(), rrr.clone(), "run config",
        "2 rdot_half_nodes() + 1 at the run config's od.range_grid (rdot_nodes)", "Sec. 4.4 'the searches reported here used 27'",
        vec![check("the same at THORConfig::default()", json!(rr_def), &rrr), ran_check("/od/range_grid/rdot_nodes", &rrr)],
        None);
    let clip = json!(grid_default.clip_sigma);
    book.add("cnAttributableClipSigma", plain(grid_default.clip_sigma), clip.clone(), "config default",
        "od.range_grid.clip_sigma", "", config_checks("/od/range_grid/clip_sigma", &def, &run, &clip)[1..].to_vec(), None);
    let huber = num(&def, "/dc/huber_k");
    let hv = json!(huber);
    book.add("cnHuberK", plain(huber), hv.clone(), "config default", "dc.huber_k",
        "Sec. 4.4 'quadratic in a detection's $\\chi$ up to 3'; 'with a median $\\chi$ above 3'",
        config_checks("/dc/huber_k", &def, &run, &hv)[1..].to_vec(),
        Some("the far-start trigger is the same knob: thor_dc::refine_single holds the range first when the start's \
              median chi exceeds dc.huber_k"));
    let op = num(&def, "/dc/max_outlier_passes");
    let opv = json!(op);
    book.add("cnOutlierPassesWord", word(int(op)), opv.clone(), "config default", "dc.max_outlier_passes",
        "Sec. 4.4 'at most three times'", config_checks("/dc/max_outlier_passes", &def, &run, &opv)[1..].to_vec(), None);
    let acc = num(&def, "/od_filter/rchi2_threshold");
    let accv = json!(acc);
    book.add("cnAcceptRChiSq", plain(acc), accv.clone(), "config default", "od_filter.rchi2_threshold",
        "Sec. 4.4 'reduced $\\chi^{2}$ is at most 3'",
        config_checks("/od_filter/rchi2_threshold", &def, &run, &accv)[1..].to_vec(), None);

    // ── The sizing function (Section 5) ────────────────────────────────────────────────────────────────────────────
    book.add("cnPatchSideDeg", plain(PATCH_SIDE_DEG), json!(PATCH_SIDE_DEG), "public const",
        "thor::clustering::background::PATCH_SIDE_DEG", "Sec. 5.1 '$s = 0.2^{\\circ}$'", vec![], None);
    let floor_arcsec = BACKGROUND_ASTROMETRIC_FLOOR_MIN * 3600.0;
    book.add("cnAstrometricFloorArcsec", plain(floor_arcsec), json!(floor_arcsec), "public const",
        "thor::clustering::adaptive::BACKGROUND_ASTROMETRIC_FLOOR_MIN (deg) * 3600", "Sec. 5.1 'and $1''$'", vec![], None);
    // E grows as R^(2k - 4) (Sec. 5.1's own scaling) at the rule's k.
    let pow_partial = 2 * PARTIAL_RULE.min_obs as u64 - 4;
    let pow_crit = 2 * crit.min_obs as u64 - 4;
    book.add("cnScalingPowerPartial", pow_partial.to_string(), json!(pow_partial), "derived",
        "2k - 4 at PARTIAL_RULE.min_obs", "Sec. 5.1 'rise as $R^{4}$ at small radii'", vec![], None);
    book.add("cnScalingPowerPartialOrdinal", ordinal(pow_partial), json!(pow_partial), "derived",
        "2k - 4 at PARTIAL_RULE.min_obs", "Sec. 5.1 'as the fourth power of $R$'", vec![], None);
    book.add("cnScalingPowerCriterionOrdinal", ordinal(pow_crit), json!(pow_crit), "derived",
        "2k - 4 at thor::criterion::Criterion::default().min_obs", "Sec. 5.1 'and the eighth for six on four'", vec![], None);
    let mut ident = [[0.0f64; 6]; 6];
    for (i, row) in ident.iter_mut().enumerate() {
        row[i] = 1.0;
    }
    let edges = sigma_point_offsets(&ident, n).len() as u64;
    book.add("cnDriftEdgeStatesWordCap", capitalised(&word(edges)), json!(edges), "public fn",
        "thor::sizing::sigma_point_offsets(full-rank covariance, n).len() (PropagatorDrift's edge states)",
        "Sec. 5.2 'Twelve states'", vec![],
        Some("the offsets are +-n along each column of the Cholesky factor of Sigma_0"));
    book.add("cnLadderRatio", plain(LADDER_RATIO), json!(LADDER_RATIO), "public const", "thor::sizing::LADDER_RATIO",
        "Sec. 5.2 'each rung $0.8$ of the one above'", vec![], None);
    book.add("cnTwoNightsDays", plain(TWO_NIGHTS_DAYS), json!(TWO_NIGHTS_DAYS), "public const",
        "thor::sizing::TWO_NIGHTS_DAYS", "Sec. 5.3 prints 'at least a day', 'shorter than a day'", vec![], None);
    let min_hours = WINDOW_MIN_DAYS * 24.0;
    book.add("cnWindowMinHoursWord", word(int(min_hours)), json!(min_hours), "public const",
        "thor::sizing::WINDOW_MIN_DAYS * 24", "Sec. 5.3 'down to one hour'", vec![], None);

    // ── Reproducing this paper (Appendix D) ────────────────────────────────────────────────────────────────────────
    let pin = env!("THOR_PIN");
    book.add("cnThorCommit", pin[..8].to_string(), json!(pin), "build", "thor.rev (THOR_PIN, checked against vendor/thor-rs)",
        "App. D 'made at \\thor\\ commit 939d286e'", vec![], None);

    // ── What THOR holds privately or hard-codes: named, not copied ─────────────────────────────────────────────────
    let not_sourced = json!([
        {"constant": "eta thresholds 0.01 and 0.25", "file": "src/transform.rs",
         "symbol": "project_obs_covariance_escalating (private fn): local consts EPS1, EPS2", "text": "Sec. 3.4"},
        {"constant": "unscented transform: five sigma points, Cholesky of 3 Sigma_d, fewer than three survivors fall back",
         "file": "src/transform.rs",
         "symbol": "sigma_point_project_cov (private fn): local consts N = 2, NL = 3; `proj.len() < 3`", "text": "Sec. 3.4"},
        {"constant": "night = floor(t + lambda/360 - 0.5)", "file": "src/observations/obscodes.rs",
         "symbol": "ObscodeLongitudes::night_utc (public fn, inline literals 360.0 and 0.5)", "text": "Sec. 2.3"},
        {"constant": "the seed's quadratic: at least three nights, curvature above half the radius",
         "file": "src/pipeline/stages/seeds.rs",
         "symbol": "OrderRule::order (private): `nights >= 3`, `sag > 0.5 * self.radius`", "text": "Sec. 4.3"},
        {"constant": "the attributable: a cubic, a quadratic below eight detections",
         "file": "src/orbit_determination/range_grid.rs",
         "symbol": "attributable_fit (private fn): `if t.len() >= 8 { 3 } else { 2 }`", "text": "Sec. 4.4"},
        {"constant": "accepted orbits sharing 70 percent of their detections reduced to one",
         "file": "src/pipeline/stages/window_loop.rs",
         "symbol": "od_batch (private fn): Jaccard of selected member sets `>= 0.7`", "text": "Sec. 4.4"},
        {"constant": "same-night repeats within half a radius", "file": "src/pipeline/stages/cluster.rs",
         "symbol": "collapse_repeats (public fn, inline literal `0.5 * c.r_cluster_deg`)", "text": "Sec. 5.1"},
        {"constant": "the attributable clip's 1 arcsec floor", "file": "src/orbit_determination/range_grid.rs",
         "symbol": "inline `floor_rad = (1.0 / 3600.0_f64).to_radians()` in the attributable clip", "text": ""},
        {"constant": "physical bounds (r 0.01-1000 au, v <= 2 au/day, ...)", "file": "src/orbit_determination/filter.rs",
         "symbol": "physical_rejection_reason (public fn, inline literals)", "text": ""},
        {"constant": "THOR 1's 300 x 300 velocities over -0.1 to 0.1 deg/day; DBSCAN at 0.005 deg and five detections",
         "file": "not in THOR 2", "symbol": "THOR 1 (Python, Moeyens et al. 2021 Section 3.1)", "text": "Sec. 4.1"},
    ]);

    // ── The record ─────────────────────────────────────────────────────────────────────────────────────────────────
    for j in &job_records {
        inputs.push(json!({"role": "r07dp2 job", "record": j}));
    }
    let macros_json: Vec<Value> = book
        .macros
        .iter()
        .map(|m| {
            json!({"name": format!("\\{}", m.name), "tex": m.tex, "value": m.value, "kind": m.kind, "source": m.source,
                   "printed": !m.text.is_empty(), "text": m.text, "checks": m.checks, "note": m.note})
        })
        .collect();
    let record = json!({
        "experiment": "constants",
        "experiments_commit": env!("EXPERIMENTS_COMMIT"), "experiments_source": env!("EXPERIMENTS_SOURCE"),
        "thor_pin": pin, "thor_source": env!("THOR_SOURCE"),
        "text_as_of": TEXT_AS_OF,
        "sources": "in order of preference: a public const or fn of THOR; THORConfig::default(), serialised; the run \
                    config of r07dp2 for values the reported searches set. Every macro lists the other places THOR states \
                    the same value under `checks`, with whether each agrees.",
        "inputs": inputs,
        "run_config": {
            "path": args.run_config.display().to_string(),
            "loads_at_pin": true,
            "fields_removed_from_the_job_config": removed,
            "what_ran": {
                "statement": "r07dp2's grid contract rewrote keys of this config for every test orbit. \
                              `contract_overrides` applies each job's own contract to this file with GridContract::apply \
                              at the pin; the run itself ran the THOR of `thor_image`. \
                              `run_json_config_snapshot_at_overridden_keys` says what each run.json's config_snapshot \
                              holds at those keys.",
                "thor_image": images,
                "jobs": n_jobs, "finished_jobs_with_run_json": n_finished,
                "contract_overrides": tally_json(override_counts),
                "run_json_policies": tally_json(policy_counts),
                "run_json_config_snapshot_at_overridden_keys": tally_json(snapshot_counts),
                "job_config_keys_differing_from_this_file": tally_json(job_config_diff_counts),
            },
        },
        "macros": macros_json,
        "code_disagreements": book.disagreements,
        "not_sourced": not_sourced,
    });
    if let Some(dir) = args.out_json.parent() {
        fs::create_dir_all(dir).ok();
    }
    fs::write(&args.out_json, serde_json::to_string_pretty(&record).unwrap()).expect("write json");

    // Numbers for the manuscript, as LaTeX macros: never typed by hand.
    let exp = env!("EXPERIMENTS_COMMIT_SHORT");
    let mut tex = format!(
        "%% Generated by thor_experiments/thor_paper2/src/bin/constants.rs (thor_experiments {exp}, THOR {}); do not edit.\n\
         %% THOR's design constants, read from THOR at its pin; results/constants.json names each one's source.\n",
        &pin[..8]
    );
    for m in &book.macros {
        tex.push_str(&format!("\\newcommand{{\\{}}}{{{}}}\n", m.name, m.tex));
    }
    if let Some(dir) = args.out_tex.parent() {
        fs::create_dir_all(dir).ok();
    }
    fs::write(&args.out_tex, tex).expect("write tex");
    eprintln!(
        "{} macros; {} code disagreements; {} jobs ({} finished); contract overrides: {}",
        book.macros.len(),
        book.disagreements.len(),
        n_jobs,
        n_finished,
        serde_json::to_string(&record["run_config"]["what_ran"]["contract_overrides"]).unwrap()
    );
}
