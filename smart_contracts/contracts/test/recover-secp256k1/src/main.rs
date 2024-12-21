#![no_std]
#![no_main]

extern crate alloc;
use alloc::string::String;
use casper_contract::{
    contract_api::{cryptography, runtime},
    unwrap_or_revert::UnwrapOrRevert,
};
use casper_types::{
    bytesrepr::{Bytes, FromBytes},
    PublicKey, Signature,
};

const ARG_MESSAGE: &str = "message";
const ARG_SIGNATURE_BYTES: &str = "signature_bytes";
const ARG_RECOVERY_ID: &str = "recovery_id";
const ARG_EXPECTED: &str = "expected";

#[no_mangle]
pub extern "C" fn call() {
    let message: String = runtime::get_named_arg(ARG_MESSAGE);
    let signature_bytes: Bytes = runtime::get_named_arg(ARG_SIGNATURE_BYTES);
    let recovery_id: u8 = runtime::get_named_arg(ARG_RECOVERY_ID);
    let expected: PublicKey = runtime::get_named_arg(ARG_EXPECTED);

    let (signature, _) = Signature::from_bytes(&signature_bytes).unwrap();
    let recovered_pk = cryptography::recover_secp256k1(message.as_bytes(), &signature, recovery_id)
        .unwrap_or_revert();

    assert_eq!(recovered_pk, expected, "PublicKey mismatch");
}
