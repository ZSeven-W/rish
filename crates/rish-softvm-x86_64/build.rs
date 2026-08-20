//! Records the source revision of the rish workspace for the pure-Rust
//! provider build identity (evidence chain). An explicit
//! RISH_SOURCE_REVISION environment variable wins; otherwise the build asks
//! git for the current HEAD and falls back to an unpinned marker so source
//! tarball builds still compile (the gate only rejects empty revisions).

fn main() {
    println!("cargo:rerun-if-env-changed=RISH_SOURCE_REVISION");
    let revision = std::env::var("RISH_SOURCE_REVISION")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(git_head);
    println!("cargo:rustc-env=RISH_SOURCE_REVISION={revision}");
}

fn git_head() -> String {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output();
    match output {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        }
        _ => format!("unpinned-{}", env!("CARGO_PKG_VERSION")),
    }
}
