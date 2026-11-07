include!("../../../tests/conformance/runner.rs");

#[test]
fn conformance() {
    run_suite(env!("CARGO_BIN_EXE_ns-linux-v8"));
}
