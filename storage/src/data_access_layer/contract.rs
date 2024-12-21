use crate::tracking_copy::TrackingCopyError;
use casper_types::{Contract, Digest, Key};

/// Represents a request to obtain contract.
pub struct ContractRequest {
    state_hash: Digest,
    key: Key,
}

impl ContractRequest {
    /// ctor
    pub fn new(state_hash: Digest, key: Key) -> Self {
        ContractRequest { state_hash, key }
    }

    /// Returns key.
    pub fn key(&self) -> Key {
        self.key
    }
    /// Returns state root hash.
    pub fn state_hash(&self) -> Digest {
        self.state_hash
    }
}

/// Represents a result of a `contract` request.
#[derive(Debug)]
pub enum ContractResult {
    /// Invalid state root hash.
    RootNotFound,
    /// Value not found.
    ValueNotFound(String),
    /// This variant will be returned if the contract was found.
    Success {
        /// A contract.
        contract: Contract,
    },
    /// Failure result.
    Failure(TrackingCopyError),
}
