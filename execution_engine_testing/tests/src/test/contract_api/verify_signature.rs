use casper_engine_test_support::{
    ExecuteRequestBuilder, LmdbWasmTestBuilder, DEFAULT_ACCOUNT_ADDR, LOCAL_GENESIS_REQUEST,
};
use casper_types::{
    bytesrepr::{Bytes, ToBytes},
    runtime_args, PublicKey, SecretKey, Signature,
};
use ed25519_dalek::Signer;

const VERIFY_SIGNATURE_WASM: &str = "verify_signature.wasm";

#[ignore]
#[test]
fn should_verify_secp256k1_signature() {
    let message = String::from("Recovery test");
    let message_bytes = message.as_bytes();
    let signing_key = SecretKey::generate_secp256k1().unwrap();
    let public_key = PublicKey::from(&signing_key);

    let (signature, _) = match signing_key {
        SecretKey::Secp256k1(signing_key) => signing_key.sign_recoverable(message_bytes).unwrap(),
        _ => panic!("Expected a Secp256k1 key"),
    };

    let signature = Signature::Secp256k1(signature);
    let signature_bytes: Bytes = signature.to_bytes().unwrap().into();

    LmdbWasmTestBuilder::default()
        .run_genesis(LOCAL_GENESIS_REQUEST.clone())
        .exec(
            ExecuteRequestBuilder::standard(
                *DEFAULT_ACCOUNT_ADDR,
                VERIFY_SIGNATURE_WASM,
                runtime_args! {
                    "message" => message,
                    "signature_bytes" => signature_bytes,
                    "public_key" => public_key,
                },
            )
            .build(),
        )
        .expect_success()
        .commit();
}

#[ignore]
#[test]
fn should_verify_ed25519_signature() {
    let message = String::from("Recovery test");
    let message_bytes = message.as_bytes();
    let signing_key = SecretKey::generate_ed25519().unwrap();
    let public_key = PublicKey::from(&signing_key);

    let signature = match signing_key {
        SecretKey::Ed25519(signing_key) => signing_key.sign(message_bytes),
        _ => panic!("Expected an Ed25519 key"),
    };

    let signature = Signature::Ed25519(signature);
    let signature_bytes: Bytes = signature.to_bytes().unwrap().into();

    LmdbWasmTestBuilder::default()
        .run_genesis(LOCAL_GENESIS_REQUEST.clone())
        .exec(
            ExecuteRequestBuilder::standard(
                *DEFAULT_ACCOUNT_ADDR,
                VERIFY_SIGNATURE_WASM,
                runtime_args! {
                    "message" => message,
                    "signature_bytes" => signature_bytes,
                    "public_key" => public_key,
                },
            )
            .build(),
        )
        .expect_success()
        .commit();
}

#[ignore]
#[test]
fn should_fail_verify_secp256k1_signature() {
    let message = String::from("Recovery test");
    let message_bytes = message.as_bytes();
    let signing_key = SecretKey::generate_secp256k1().unwrap();
    let unrelated_key = PublicKey::from(&SecretKey::generate_secp256k1().unwrap());

    let (signature, _) = match signing_key {
        SecretKey::Secp256k1(signing_key) => signing_key.sign_recoverable(message_bytes).unwrap(),
        _ => panic!("Expected a Secp256k1 key"),
    };

    let signature = Signature::Secp256k1(signature);
    let signature_bytes: Bytes = signature.to_bytes().unwrap().into();

    LmdbWasmTestBuilder::default()
        .run_genesis(LOCAL_GENESIS_REQUEST.clone())
        .exec(
            ExecuteRequestBuilder::standard(
                *DEFAULT_ACCOUNT_ADDR,
                VERIFY_SIGNATURE_WASM,
                runtime_args! {
                    "message" => message,
                    "signature_bytes" => signature_bytes,
                    "public_key" => unrelated_key,
                },
            )
            .build(),
        )
        .expect_failure()
        .commit();
}

#[ignore]
#[test]
fn should_fail_verify_ed25519_signature() {
    let message = String::from("Recovery test");
    let message_bytes = message.as_bytes();
    let signing_key = SecretKey::generate_ed25519().unwrap();
    let unrelated_key = PublicKey::from(&SecretKey::generate_ed25519().unwrap());

    let signature = match signing_key {
        SecretKey::Ed25519(signing_key) => signing_key.sign(message_bytes),
        _ => panic!("Expected an Ed25519 key"),
    };

    let signature = Signature::Ed25519(signature);
    let signature_bytes: Bytes = signature.to_bytes().unwrap().into();

    LmdbWasmTestBuilder::default()
        .run_genesis(LOCAL_GENESIS_REQUEST.clone())
        .exec(
            ExecuteRequestBuilder::standard(
                *DEFAULT_ACCOUNT_ADDR,
                VERIFY_SIGNATURE_WASM,
                runtime_args! {
                    "message" => message,
                    "signature_bytes" => signature_bytes,
                    "public_key" => unrelated_key,
                },
            )
            .build(),
        )
        .expect_failure()
        .commit();
}
