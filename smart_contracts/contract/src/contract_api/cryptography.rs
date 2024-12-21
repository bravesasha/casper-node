//! Functions with cryptographic utils.

use casper_types::{
    api_error,
    bytesrepr::{FromBytes, ToBytes, U8_SERIALIZED_LENGTH},
    ApiError, HashAlgorithm, PublicKey, Signature, BLAKE2B_DIGEST_LENGTH,
};

use crate::{ext_ffi, unwrap_or_revert::UnwrapOrRevert};

/// Computes digest hash, using provided algorithm type.
pub fn generic_hash<T: AsRef<[u8]>>(input: T, algo: HashAlgorithm) -> [u8; 32] {
    let mut ret = [0; 32];

    let result = unsafe {
        ext_ffi::casper_generic_hash(
            input.as_ref().as_ptr(),
            input.as_ref().len(),
            algo as u8,
            ret.as_mut_ptr(),
            BLAKE2B_DIGEST_LENGTH,
        )
    };
    api_error::result_from(result).unwrap_or_revert();
    ret
}

/// Attempts to recover a Secp256k1 [`PublicKey`] from a message and a signature over it.
pub fn recover_secp256k1<T: AsRef<[u8]>>(
    data: T,
    signature: &Signature,
    recovery_id: u8,
) -> Result<PublicKey, ApiError> {
    let mut buffer = [0; U8_SERIALIZED_LENGTH + PublicKey::SECP256K1_LENGTH];
    let signature_bytes = signature.to_bytes().unwrap_or_revert();

    let result = unsafe {
        ext_ffi::casper_recover_secp256k1(
            data.as_ref().as_ptr(),
            data.as_ref().len(),
            signature_bytes.as_ptr(),
            signature_bytes.len(),
            buffer.as_mut_ptr(),
            recovery_id,
        )
    };

    PublicKey::from_bytes(&buffer)
        .map(|(key, _)| key)
        .map_err(|_| ApiError::from(result as u32))
}

/// Verifies the signature of the given message against the given public key.
pub fn verify_signature<T: AsRef<[u8]>>(
    data: T,
    signature: &Signature,
    public_key: &PublicKey,
) -> Result<(), ApiError> {
    let signature_bytes = signature.to_bytes().unwrap_or_revert();
    let public_key_bytes = public_key.to_bytes().unwrap_or_revert();

    let result = unsafe {
        ext_ffi::casper_verify_signature(
            data.as_ref().as_ptr(),
            data.as_ref().len(),
            signature_bytes.as_ptr(),
            signature_bytes.len(),
            public_key_bytes.as_ptr(),
            public_key_bytes.len(),
        )
    };

    api_error::result_from(result)
}
