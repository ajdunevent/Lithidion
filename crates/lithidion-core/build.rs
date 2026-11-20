fn main() {
    // Reruns, and so changes, whenever anything in this crate or the protocol crate does.
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=../pebble-protocol/src");
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    println!("cargo:rustc-env=LITHIDION_BUILD={}-{now:x}", std::env::var("CARGO_PKG_VERSION").unwrap_or_default());
}
