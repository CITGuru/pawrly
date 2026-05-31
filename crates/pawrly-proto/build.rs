//! Generate tonic bindings for every `.proto` under `proto/pawrly/v1/`.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_root = "proto";
    let protos = &[
        "proto/pawrly/v1/common.proto",
        "proto/pawrly/v1/query.proto",
        "proto/pawrly/v1/catalog.proto",
        "proto/pawrly/v1/sources.proto",
        "proto/pawrly/v1/cache.proto",
        "proto/pawrly/v1/admin.proto",
        "proto/pawrly/v1/semantic.proto",
    ];

    println!("cargo:rerun-if-changed={proto_root}");
    for p in protos {
        println!("cargo:rerun-if-changed={p}");
    }

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(protos, &[proto_root])?;

    Ok(())
}
