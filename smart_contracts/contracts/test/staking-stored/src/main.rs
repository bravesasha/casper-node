#![no_std]
#![no_main]

extern crate alloc;

use alloc::{string::ToString, vec};

use casper_contract::{
    contract_api::{account, runtime, storage, system},
    unwrap_or_revert::UnwrapOrRevert,
};
use casper_types::{
    contracts::NamedKeys, ApiError, CLType, EntryPoint, EntryPointAccess, EntryPointPayment,
    EntryPointType, EntryPoints, Key, Parameter, URef,
};

#[repr(u16)]
enum InstallerSessionError {
    FailedToTransfer = 101,
}

#[no_mangle]
pub extern "C" fn call_staking() {
    staking::run();
}

fn build_named_keys_and_purse() -> (NamedKeys, URef) {
    let mut named_keys = NamedKeys::new();
    let purse = system::create_purse();

    named_keys.insert(staking::STAKING_PURSE.to_string(), purse.into());
    named_keys.insert(staking::INSTALLER.to_string(), runtime::get_caller().into());

    (named_keys, purse)
}

fn entry_points() -> EntryPoints {
    let mut entry_points = EntryPoints::new();

    entry_points.add_entry_point(EntryPoint::new(
        staking::ENTRY_POINT_RUN,
        vec![
            Parameter::new(staking::ARG_ACTION, CLType::String),
            Parameter::new(staking::ARG_AMOUNT, CLType::U512),
            Parameter::new(staking::ARG_VALIDATOR, CLType::PublicKey),
            Parameter::new(staking::ARG_NEW_VALIDATOR, CLType::PublicKey),
        ],
        CLType::Unit,
        EntryPointAccess::Public,
        EntryPointType::Called,
        EntryPointPayment::Caller,
    ));

    entry_points
}

#[no_mangle]
pub extern "C" fn call() {
    let entry_points = entry_points();

    let (staking_named_keys, staking_purse) = build_named_keys_and_purse();

    let (contract_hash, contract_version) = storage::new_contract(
        entry_points,
        Some(staking_named_keys),
        Some(staking::HASH_KEY_NAME.to_string()),
        Some(staking::ACCESS_KEY_NAME.to_string()),
        None,
    );

    runtime::put_key(
        staking::CONTRACT_VERSION,
        storage::new_uref(contract_version).into(),
    );

    runtime::put_key(staking::CONTRACT_NAME, Key::Hash(contract_hash.value()));

    // Initial funding amount.
    let amount = runtime::get_named_arg(staking::ARG_AMOUNT);
    system::transfer_from_purse_to_purse(account::get_main_purse(), staking_purse, amount, None)
        .unwrap_or_revert_with(ApiError::User(
            InstallerSessionError::FailedToTransfer as u16,
        ));
}
