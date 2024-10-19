fn main() {
    tonic_build::configure()
        .out_dir("src/pb") // Output directory for the generated Rust files
        .compile(&["proto/voip_service_grpc.proto"], &["proto"]) // Proto file and its directory
        .unwrap_or_else(|e| panic!("Failed to compile protos {:?}", e));

    println!("cargo:rerun-if-changed=proto/voip_service_grpc.proto");
}
