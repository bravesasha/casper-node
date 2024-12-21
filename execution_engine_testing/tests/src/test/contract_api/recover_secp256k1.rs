use casper_engine_test_support::{
    ExecuteRequestBuilder, LmdbWasmTestBuilder, DEFAULT_ACCOUNT_ADDR, LOCAL_GENESIS_REQUEST,
};
use casper_types::{
    bytesrepr::{Bytes, ToBytes},
    runtime_args, PublicKey, SecretKey, Signature,
};

const RECOVER_SECP256K1_WASM: &str = "recover_secp256k1.wasm";

#[ignore]
#[test]
fn should_recover_secp256k1() {
    let message = String::from("Recovery test");
    let message_bytes = message.as_bytes();
    let signing_key = SecretKey::generate_secp256k1().unwrap();
    let public_key = PublicKey::from(&signing_key);

    let (signature, recovery_id) = match signing_key {
        SecretKey::Secp256k1(signing_key) => signing_key.sign_recoverable(message_bytes).unwrap(),
        _ => panic!("PK recovery mechanism only works with Secp256k1 keys"),
    };

    let signature = Signature::Secp256k1(signature);
    let signature_bytes: Bytes = signature.to_bytes().unwrap().into();
    let recovery_id = recovery_id.to_byte();

    LmdbWasmTestBuilder::default()
        .run_genesis(LOCAL_GENESIS_REQUEST.clone())
        .exec(
            ExecuteRequestBuilder::standard(
                *DEFAULT_ACCOUNT_ADDR,
                RECOVER_SECP256K1_WASM,
                runtime_args! {
                    "message" => message,
                    "signature_bytes" => signature_bytes,
                    "recovery_id" => recovery_id,
                    "expected" => public_key
                },
            )
            .build(),
        )
        .expect_success()
        .commit();
}

#[ignore]
#[test]
fn should_fail_recover_secp256k1_recovery_id_out_of_range() {
    let message = String::from("Recovery test");
    let message_bytes = message.as_bytes();
    let signing_key = SecretKey::generate_secp256k1().unwrap();
    let public_key = PublicKey::from(&signing_key);

    let (signature, _) = match signing_key {
        SecretKey::Secp256k1(signing_key) => signing_key.sign_recoverable(message_bytes).unwrap(),
        _ => panic!("PK recovery mechanism only works with Secp256k1 keys"),
    };

    let signature = Signature::Secp256k1(signature);
    let signature_bytes: Bytes = signature.to_bytes().unwrap().into();
    let recovery_id = 4;

    LmdbWasmTestBuilder::default()
        .run_genesis(LOCAL_GENESIS_REQUEST.clone())
        .exec(
            ExecuteRequestBuilder::standard(
                *DEFAULT_ACCOUNT_ADDR,
                RECOVER_SECP256K1_WASM,
                runtime_args! {
                    "message" => message,
                    "signature_bytes" => signature_bytes,
                    "recovery_id" => recovery_id,
                    "expected" => public_key
                },
            )
            .build(),
        )
        .expect_failure()
        .commit();
}

#[ignore]
#[test]
fn should_fail_recover_secp256k1_pk_mismatch() {
    let message = String::from("Recovery test");
    let message_bytes = message.as_bytes();
    let signing_key = SecretKey::generate_secp256k1().unwrap();

    let (signature, _) = match signing_key {
        SecretKey::Secp256k1(signing_key) => signing_key.sign_recoverable(message_bytes).unwrap(),
        _ => panic!("PK recovery mechanism only works with Secp256k1 keys"),
    };

    let signature = Signature::Secp256k1(signature);
    let signature_bytes: Bytes = signature.to_bytes().unwrap().into();
    let recovery_id = 4;

    LmdbWasmTestBuilder::default()
        .run_genesis(LOCAL_GENESIS_REQUEST.clone())
        .exec(
            ExecuteRequestBuilder::standard(
                *DEFAULT_ACCOUNT_ADDR,
                RECOVER_SECP256K1_WASM,
                runtime_args! {
                    "message" => message,
                    "signature_bytes" => signature_bytes,
                    "recovery_id" => recovery_id,
                    "expected" => PublicKey::System
                },
            )
            .build(),
        )
        .expect_failure()
        .commit();
}
