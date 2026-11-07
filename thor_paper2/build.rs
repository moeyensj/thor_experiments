//! Records which THOR every experiment was built against: the commit in thor.rev, checked against the marker
//! fetch_thor.sh leaves in vendor/thor-rs, so a result can never claim a THOR it was not built from. With the assist
//! feature it also records the ASSIST crates the build resolved (name, version and source, read from Cargo.lock).
//! It records this repository's commit too, with "-dirty" when the code here has uncommitted changes.
use std::fs;

fn main() {
    println!("cargo:rerun-if-changed=thor.rev");
    println!("cargo:rerun-if-changed=vendor/thor-rs/.thor-rev");
    let pin = fs::read_to_string("thor.rev").expect("thor.rev").trim().to_string();
    let built = fs::read_to_string("vendor/thor-rs/.thor-rev").unwrap_or_default().trim().to_string();
    if built != pin {
        panic!("vendor/thor-rs is THOR {built:?}, but thor.rev pins {pin}: run ./fetch_thor.sh");
    }
    println!("cargo:rustc-env=THOR_PIN={pin}");
    println!("cargo:rustc-env=THOR_SOURCE=git@github.com:moeyensj/thor_rust.git");

    // This repository's commit, so a result names the code that made it. A build outside a git checkout stops here.
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git").args(args).output().expect("git");
        assert!(out.status.success(), "git {args:?} failed: thor_paper2 builds from a checkout of thor_experiments");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let commit = git(&["rev-parse", "HEAD"]);
    let code = ["src", "build.rs", "Cargo.toml", "Cargo.lock", "thor.rev"];
    let dirty = !git(&[&["status", "--porcelain", "--"][..], &code[..]].concat()).is_empty();
    let suffix = if dirty { "-dirty" } else { "" };
    println!("cargo:rustc-env=EXPERIMENTS_COMMIT={commit}{suffix}");
    println!("cargo:rustc-env=EXPERIMENTS_COMMIT_SHORT={}{suffix}", &commit[..8]);
    println!("cargo:rustc-env=EXPERIMENTS_SOURCE=git@github.com:moeyensj/thor_experiments.git");
    let branch = git(&["rev-parse", "--symbolic-full-name", "HEAD"]);
    for p in ["HEAD", "index", branch.as_str()] {
        println!("cargo:rerun-if-changed={}", git(&["rev-parse", "--git-path", p]));
    }
    println!("cargo:rerun-if-changed=src");

    // The ASSIST crates as resolved: one `name version source` entry per package, `;`-separated. A path source (the
    // vendored, patched copies) has no `source` line in the lockfile and is recorded as "path".
    println!("cargo:rerun-if-changed=Cargo.lock");
    let lock = fs::read_to_string("Cargo.lock").unwrap_or_default();
    let mut found = Vec::new();
    for block in lock.split("[[package]]") {
        let field = |k: &str| {
            block.lines().find_map(|l| l.strip_prefix(&format!("{k} = \"")).map(|v| v.trim_end_matches('"').to_string()))
        };
        if let (Some(name), Some(version)) = (field("name"), field("version")) {
            if ["assist-rs", "libassist-sys", "thor-rs-assist"].contains(&name.as_str()) {
                found.push(format!("{name} {version} {}", field("source").unwrap_or_else(|| "path".into())));
            }
        }
    }
    println!("cargo:rustc-env=ASSIST_CRATES={}", found.join(";"));
}
