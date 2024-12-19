#![no_std]
#![no_main]

extern crate alloc;

use casper_contract::contract_api::{runtime, runtime::put_key, system};
use casper_types::{RuntimeArgs, URef};

const GET_PAYMENT_PURSE: &str = "get_payment_purse";
const THIS_SHOULD_FAIL: &str = "this_should_fail";

/// This logic is intended to be used as SESSION PAYMENT LOGIC
/// It gets the payment purse and attempts and attempts to persist it,
/// which should fail.
#[no_mangle]
pub extern "C" fn call() {
    // handle payment contract
    let handle_payment_contract_hash = system::get_handle_payment();

    // get payment purse for current execution
    let payment_purse: URef = runtime::call_contract(
        handle_payment_contract_hash,
        GET_PAYMENT_PURSE,
        RuntimeArgs::default(),
    );
    // attempt to persist the payment purse, which should fail
    put_key(THIS_SHOULD_FAIL, payment_purse.into());
}
