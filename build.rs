// Rebuild when migrations change: they are embedded in the binary by `sqlx::migrate!`.
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
