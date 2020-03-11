fn main() {
    prost_build::compile_protos(
        &[
            "src/proto/types.proto",
            "src/proto/structures.proto",
            "src/proto/responses.proto",
            "src/proto/events.proto",
            "src/proto/requests/auth.proto",
            "src/proto/requests/active.proto",
        ],
        &["src/proto"],
    )
    .unwrap();
}