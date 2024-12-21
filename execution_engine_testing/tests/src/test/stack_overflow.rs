use casper_engine_test_support::{
    ExecuteRequestBuilder, LmdbWasmTestBuilder, DEFAULT_ACCOUNT_ADDR, LOCAL_GENESIS_REQUEST,
};
use casper_execution_engine::{engine_state::Error, execution::ExecError};
use casper_types::RuntimeArgs;

#[ignore]
#[test]
fn runtime_stack_overflow_should_cause_unreachable_error() {
    // Create an unconstrained recursive call
    let wat = r#"(module
        (func $call (call $call))
        (export "call" (func $call))
        (memory $memory 1)
      )"#;

    let module_bytes = wat::parse_str(wat).unwrap();

    let do_stack_overflow_request = ExecuteRequestBuilder::module_bytes(
        *DEFAULT_ACCOUNT_ADDR,
        module_bytes,
        RuntimeArgs::default(),
    )
    .build();

    let mut builder = LmdbWasmTestBuilder::default();

    builder.run_genesis(LOCAL_GENESIS_REQUEST.clone());
    builder
        .exec(do_stack_overflow_request)
        .expect_failure()
        .commit();

    let error = builder.get_error().expect("should have error");
    assert!(
        matches!(&error, Error::Exec(ExecError::Interpreter(s)) if s.contains("Unreachable")),
        "{:?}",
        error
    );
}
