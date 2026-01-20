fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=src/proto/slot_lock.proto");
    println!("cargo:rerun-if-changed=src/proto/health.proto");

    tonic_build::configure().compile_protos(
        &["src/proto/slot_lock.proto", "src/proto/health.proto"],
        &["src/proto"],
    )?;
    Ok(())
}
