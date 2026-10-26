fn main() {
    println!("cargo:rerun-if-changed=assets/zest.rc");
    println!("cargo:rerun-if-changed=assets/zest.ico");
    println!("cargo:rerun-if-changed=assets/zest.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("assets/zest.rc", embed_resource::NONE)
            .manifest_required()
            .unwrap();
    }
}
