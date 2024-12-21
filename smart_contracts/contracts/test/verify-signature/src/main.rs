#![no_std]
#![no_main]

extern crate alloc;
use alloc::string::String;
use casper_contract::contract_api::{cryptography, runtime};
use casper_types::{
    bytesrepr::{Bytes, FromBytes},
    PublicKey, Signature,
};

const ARG_MESSAGE: &str = "message";
const ARG_SIGNATURE_BYTES: &str = "signature_bytes";
const ARG_PUBLIC_KEY: &str = "public_key";

#[no_mangle]
pub extern "C" fn call() {
    let message: String = runtime::get_named_arg(ARG_MESSAGE);
    let signature_bytes: Bytes = runtime::get_named_arg(ARG_SIGNATURE_BYTES);
    let public_key: PublicKey = runtime::get_named_arg(ARG_PUBLIC_KEY);

    let (signature, _) = Signature::from_bytes(&signature_bytes).unwrap();
    let verify = cryptography::verify_signature(message.as_bytes(), &signature, &public_key);

    assert!(verify.is_ok());
}
