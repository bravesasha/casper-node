#![no_std]

extern crate alloc;

use alloc::{
    string::{String, ToString},
    vec,
    vec::Vec,
};

use casper_contract::{
    contract_api::{runtime, runtime::revert, system},
    ext_ffi,
    unwrap_or_revert::UnwrapOrRevert,
};
use casper_types::{
    account::AccountHash,
    api_error,
    bytesrepr::{self, ToBytes},
    runtime_args,
    system::auction,
    ApiError, Key, PublicKey, URef, U512,
};

pub const STAKING_ID: &str = "staking_contract";

pub const ARG_ACTION: &str = "action";
pub const ARG_AMOUNT: &str = "amount";
pub const ARG_VALIDATOR: &str = "validator";
pub const ARG_NEW_VALIDATOR: &str = "new_validator";

pub const STAKING_PURSE: &str = "staking_purse";
pub const INSTALLER: &str = "installer";
pub const CONTRACT_NAME: &str = "staking";
pub const HASH_KEY_NAME: &str = "staking_package";
pub const ACCESS_KEY_NAME: &str = "staking_package_access";
pub const CONTRACT_VERSION: &str = "staking_contract_version";
pub const ENTRY_POINT_RUN: &str = "run";

#[repr(u16)]
enum StakingError {
    InvalidAccount = 1,
    MissingInstaller = 2,
    InvalidInstaller = 3,
    MissingStakingPurse = 4,
    InvalidStakingPurse = 5,
    UnexpectedKeyVariant = 6,
    UnexpectedAction = 7,
    MissingValidator = 8,
    MissingNewValidator = 9,
}

impl From<StakingError> for ApiError {
    fn from(e: StakingError) -> Self {
        ApiError::User(e as u16)
    }
}

#[no_mangle]
pub fn run() {
    let caller = runtime::get_caller();
    let installer = get_account_hash_with_user_errors(
        INSTALLER,
        StakingError::MissingInstaller,
        StakingError::InvalidInstaller,
    );

    if caller != installer {
        revert(ApiError::User(StakingError::InvalidAccount as u16));
    }

    let action: String = runtime::get_named_arg(ARG_ACTION);

    if action == *"UNSTAKE".to_string() {
        unstake();
    } else if action == *"STAKE".to_string() {
        stake();
    } else if action == *"RESTAKE".to_string() {
        restake();
    } else {
        revert(ApiError::User(StakingError::UnexpectedAction as u16));
    }
}

fn unstake() {
    let args = get_unstaking_args(false);
    let contract_hash = system::get_auction();
    runtime::call_contract::<U512>(contract_hash, auction::METHOD_UNDELEGATE, args);
}

fn restake() {
    let args = get_unstaking_args(true);
    let contract_hash = system::get_auction();
    runtime::call_contract::<U512>(contract_hash, auction::METHOD_REDELEGATE, args);
}

fn stake() {
    let staking_purse = get_uref_with_user_errors(
        STAKING_PURSE,
        StakingError::MissingStakingPurse,
        StakingError::InvalidStakingPurse,
    );
    let validator: PublicKey = match runtime::try_get_named_arg(ARG_VALIDATOR) {
        Some(validator_public_key) => validator_public_key,
        None => revert(ApiError::User(StakingError::MissingValidator as u16)),
    };
    let amount: U512 = runtime::get_named_arg(ARG_AMOUNT);
    let contract_hash = system::get_auction();
    let args = runtime_args! {
        auction::ARG_DELEGATOR_PURSE => staking_purse,
        auction::ARG_VALIDATOR => validator,
        auction::ARG_AMOUNT => amount,
    };
    runtime::call_contract::<U512>(contract_hash, auction::METHOD_DELEGATE, args);
}

fn get_unstaking_args(is_restake: bool) -> casper_types::RuntimeArgs {
    let staking_purse = get_uref_with_user_errors(
        STAKING_PURSE,
        StakingError::MissingStakingPurse,
        StakingError::InvalidStakingPurse,
    );
    let validator: PublicKey = match runtime::try_get_named_arg(ARG_VALIDATOR) {
        Some(validator_public_key) => validator_public_key,
        None => revert(ApiError::User(StakingError::MissingValidator as u16)),
    };
    let amount: U512 = runtime::get_named_arg(ARG_AMOUNT);
    if !is_restake {
        return runtime_args! {
            auction::ARG_DELEGATOR_PURSE => staking_purse,
            auction::ARG_VALIDATOR => validator,
            auction::ARG_AMOUNT => amount,
        };
    }

    let new_validator: PublicKey = match runtime::try_get_named_arg(ARG_NEW_VALIDATOR) {
        Some(validator_public_key) => validator_public_key,
        None => revert(ApiError::User(StakingError::MissingNewValidator as u16)),
    };

    runtime_args! {
        auction::ARG_DELEGATOR_PURSE => staking_purse,
        auction::ARG_VALIDATOR => validator,
        auction::ARG_NEW_VALIDATOR => new_validator,
        auction::ARG_AMOUNT => amount,
    }
}

fn get_account_hash_with_user_errors(
    name: &str,
    missing: StakingError,
    invalid: StakingError,
) -> AccountHash {
    let key = get_key_with_user_errors(name, missing, invalid);
    key.into_account()
        .unwrap_or_revert_with(StakingError::UnexpectedKeyVariant)
}

fn get_uref_with_user_errors(name: &str, missing: StakingError, invalid: StakingError) -> URef {
    let key = get_key_with_user_errors(name, missing, invalid);
    key.into_uref()
        .unwrap_or_revert_with(StakingError::UnexpectedKeyVariant)
}

fn get_key_with_user_errors(name: &str, missing: StakingError, invalid: StakingError) -> Key {
    let (name_ptr, name_size, _bytes) = to_ptr(name);
    let mut key_bytes = vec![0u8; Key::max_serialized_length()];
    let mut total_bytes: usize = 0;
    let ret = unsafe {
        ext_ffi::casper_get_key(
            name_ptr,
            name_size,
            key_bytes.as_mut_ptr(),
            key_bytes.len(),
            &mut total_bytes as *mut usize,
        )
    };
    match api_error::result_from(ret) {
        Ok(_) => {}
        Err(ApiError::MissingKey) => revert(missing),
        Err(e) => revert(e),
    }
    key_bytes.truncate(total_bytes);

    bytesrepr::deserialize(key_bytes).unwrap_or_revert_with(invalid)
}

fn to_ptr<T: ToBytes>(t: T) -> (*const u8, usize, Vec<u8>) {
    let bytes = t.into_bytes().unwrap_or_revert();
    let ptr = bytes.as_ptr();
    let size = bytes.len();
    (ptr, size, bytes)
}
