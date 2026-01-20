fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=../proto/src/proto/slot_lock.proto");
    tonic_build::compile_protos("../proto/src/proto/slot_lock.proto")?;
    Ok(())
}
