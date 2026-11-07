//! Records which THOR every experiment was built against: the commit in thor.rev, checked against the marker
//! fetch_thor.sh leaves in vendor/thor-rs, so a result can never claim a THOR it was not built from. With the assist
//! feature it also records the ASSIST crates the build resolved (name, version and source, read from Cargo.lock).
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
