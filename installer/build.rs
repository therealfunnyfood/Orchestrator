fn main() {
    embed_resource::compile("../assets/app.rc", embed_resource::NONE).manifest_optional().unwrap();
    println!("cargo:rerun-if-changed=../target/release/orchestrator.exe");
}
