use std::mem;

use casper_engine_test_support::{
    ExecuteRequestBuilder, LmdbWasmTestBuilder, DEFAULT_ACCOUNT_ADDR, LOCAL_GENESIS_REQUEST,
};
use casper_types::{
    addressable_entity::DEFAULT_ENTRY_POINT_NAME, Gas, RuntimeArgs,
    DEFAULT_CONTROL_FLOW_BR_TABLE_MULTIPLIER,
};

use walrus::{ir::BinaryOp, FunctionBuilder, InstrSeqBuilder, Module, ModuleConfig, ValType};

#[ignore]
#[test]
fn should_charge_extra_per_amount_of_br_table_elements() {
    let mut builder = LmdbWasmTestBuilder::default();
    builder.run_genesis(LOCAL_GENESIS_REQUEST.clone());

    const FIXED_BLOCK_AMOUNT: usize = 256;
    const N_ELEMENTS: u32 = 5;
    const M_ELEMENTS: u32 = 168;

    let br_table_min_elements = fixed_cost_br_table(FIXED_BLOCK_AMOUNT, N_ELEMENTS);
    let br_table_max_elements = fixed_cost_br_table(FIXED_BLOCK_AMOUNT, M_ELEMENTS);

    assert_ne!(&br_table_min_elements, &br_table_max_elements);

    let exec_request_1 = ExecuteRequestBuilder::module_bytes(
        *DEFAULT_ACCOUNT_ADDR,
        br_table_min_elements,
        RuntimeArgs::default(),
    )
    .build();

    let exec_request_2 = ExecuteRequestBuilder::module_bytes(
        *DEFAULT_ACCOUNT_ADDR,
        br_table_max_elements,
        RuntimeArgs::default(),
    )
    .build();

    builder.exec(exec_request_1).expect_success().commit();

    let gas_cost_1 = builder.last_exec_gas_consumed();

    builder.exec(exec_request_2).expect_success().commit();

    let gas_cost_2 = builder.last_exec_gas_consumed();

    assert!(
        gas_cost_2 > gas_cost_1,
        "larger br_table should cost more gas"
    );

    let br_table_cycles = 5;

    assert_eq!(
        gas_cost_2.checked_sub(gas_cost_1),
        Some(Gas::from(
            (M_ELEMENTS - N_ELEMENTS) * DEFAULT_CONTROL_FLOW_BR_TABLE_MULTIPLIER * br_table_cycles
        )),
        "the cost difference should equal to exactly the size of br_table difference "
    );
}

#[allow(dead_code)]
fn cpu_burner_br_if(iterations: i64) -> Vec<u8> {
    let mut module = Module::with_config(ModuleConfig::new());

    let _memory_id = module.memories.add_local(false, 11, None);

    let mut loop_func = FunctionBuilder::new(&mut module.types, &[ValType::I64], &[]);

    let var_counter = module.locals.add(ValType::I64);
    let var_i = module.locals.add(ValType::I64);

    loop_func
        .func_body()
        // i := 0
        .i64_const(0)
        .local_set(var_i)
        .loop_(None, |loop_| {
            let loop_id = loop_.id();
            loop_. // loop:
                // i += 1
                local_get(var_i)
                .i64_const(1)
                .binop(BinaryOp::I64Add)
                // if i < iterations {
                .local_tee(var_i)
                .local_get(var_counter)
                .binop(BinaryOp::I64LtU)
                // goto loop
                // }
                .br_if(loop_id);
        });

    let loop_func = loop_func.finish(vec![var_counter], &mut module.funcs);

    let mut call_func = FunctionBuilder::new(&mut module.types, &[], &[]);
    call_func.func_body().i64_const(iterations).call(loop_func);

    let call_func = call_func.finish(Vec::new(), &mut module.funcs);

    module.exports.add(DEFAULT_ENTRY_POINT_NAME, call_func);

    module.emit_wasm()
}

#[allow(dead_code)]
fn cpu_burner_br_table(iterations: i64) -> Vec<u8> {
    let mut module = Module::with_config(ModuleConfig::new());

    let _memory_id = module.memories.add_local(false, 11, None);

    let mut loop_func = FunctionBuilder::new(&mut module.types, &[ValType::I64], &[]);

    let param_iterations = module.locals.add(ValType::I64);
    let local_i = module.locals.add(ValType::I64);

    loop_func
        .func_body()
        // i := 0
        .i64_const(0)
        .local_set(local_i)
        .block(None, |loop_break| {
            let loop_break_id = loop_break.id();

            loop_break.loop_(None, |while_loop| {
                let while_loop_id = while_loop.id(); // loop:

                while_loop
                    .block(None, |while_loop_inner| {
                        let while_loop_inner_id = while_loop_inner.id();
                        // counter += 1
                        while_loop_inner
                            .local_get(local_i)
                            .i64_const(1)
                            .binop(BinaryOp::I64Add)
                            // switch (i < counter) {
                            .local_tee(local_i)
                            .local_get(param_iterations)
                            .binop(BinaryOp::I64LtU)
                            .br_table(
                                vec![
                                    // case 0: break;
                                    loop_break_id,
                                    // case 1: continue; (goto while_loop)
                                    while_loop_id,
                                ]
                                .into(),
                                // default: throw()
                                while_loop_inner_id,
                            );
                    })
                    // the "throw"
                    .unreachable();
            });
        });

    let loop_func = loop_func.finish(vec![param_iterations], &mut module.funcs);

    let mut call_func = FunctionBuilder::new(&mut module.types, &[], &[]);
    call_func.func_body().i64_const(iterations).call(loop_func);

    let call_func = call_func.finish(Vec::new(), &mut module.funcs);

    module.exports.add(DEFAULT_ENTRY_POINT_NAME, call_func);

    module.emit_wasm()
}

/// Creates Wasm bytes with fixed amount of `block`s but with a `br_table` of a variable size.
///
/// Gas cost of executing `fixed_cost_br_table(n + m)` should be greater than
/// `fixed_cost_br_table(n)` by exactly `br_table.entry_cost * m` iff m > 0.
fn fixed_cost_br_table(total_labels: usize, br_table_element_size: u32) -> Vec<u8> {
    assert!((br_table_element_size as usize) < total_labels);

    let mut module = Module::with_config(ModuleConfig::new());

    let _memory_id = module.memories.add_local(false, 11, None);

    let mut br_table_func = FunctionBuilder::new(&mut module.types, &[ValType::I32], &[]);

    let param_jump_label = module.locals.add(ValType::I32);

    fn recursive_block_generator(
        current_block: &mut InstrSeqBuilder,
        mut recursive_step_fn: impl FnMut(&mut InstrSeqBuilder) -> bool,
    ) {
        if !recursive_step_fn(current_block) {
            current_block.block(None, |nested_block| {
                recursive_block_generator(nested_block, recursive_step_fn);
            });
        }
    }

    br_table_func.func_body().block(None, |outer_block| {
        // Outer block becames the "default" jump label for `br_table`.
        let outer_block_id = outer_block.id();

        // Count of recursive iterations left
        let mut counter = total_labels;

        // Labels are extended with newly generated labels at each recursive step
        let mut labels = Vec::new();

        // Generates nested blocks
        recursive_block_generator(outer_block, |step| {
            // Save current nested block in labels.
            labels.push(step.id());

            if counter == 0 {
                // At the tail of this recursive generator we'll create a `br_table` with variable
                // amount of labels depending on this function parameter.
                let labels = mem::take(&mut labels);
                let sliced_labels = labels.as_slice()[..br_table_element_size as usize].to_vec();

                // Code at the tail block
                step.local_get(param_jump_label)
                    .br_table(sliced_labels.into(), outer_block_id);

                // True means this is a tail call, and we won't go deeper
                true
            } else {
                counter -= 1;
                // Go deeper
                false
            }
        })
    });

    let br_table_func = br_table_func.finish(vec![param_jump_label], &mut module.funcs);

    let mut call_func = FunctionBuilder::new(&mut module.types, &[], &[]);
    call_func
        .func_body()
        // Call `br_table_func` with 0 as the jump label,
        // Specific value does not change the cost, so as long as it will generate valid wasm it's
        // ok.
        .i32_const(0)
        .call(br_table_func);

    let call_func = call_func.finish(Vec::new(), &mut module.funcs);

    module.exports.add(DEFAULT_ENTRY_POINT_NAME, call_func);

    module.emit_wasm()
}
