use crate::tracking_copy::TrackingCopyError;
use casper_types::{Digest, EntryPointValue, HashAddr};

/// Represents a request to obtain entry point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryPointRequest {
    state_hash: Digest,
    entry_point_name: String,
    contract_hash: HashAddr,
}

impl EntryPointRequest {
    /// ctor
    pub fn new(state_hash: Digest, entry_point_name: String, contract_hash: HashAddr) -> Self {
        EntryPointRequest {
            state_hash,
            entry_point_name,
            contract_hash,
        }
    }

    /// Returns state root hash.
    pub fn state_hash(&self) -> Digest {
        self.state_hash
    }

    /// Returns entry_point_name.
    pub fn entry_point_name(&self) -> &str {
        &self.entry_point_name
    }

    /// Returns contract_hash.
    pub fn contract_hash(&self) -> HashAddr {
        self.contract_hash
    }
}

impl From<EntryPointExistsRequest> for EntryPointRequest {
    fn from(value: EntryPointExistsRequest) -> Self {
        EntryPointRequest {
            state_hash: value.state_hash,
            entry_point_name: value.entry_point_name,
            contract_hash: value.contract_hash,
        }
    }
}

/// Represents a result of a `entry_point` request.
#[derive(Debug)]
pub enum EntryPointResult {
    /// Invalid state root hash.
    RootNotFound,
    /// Value not found.
    ValueNotFound(String),
    /// Contains an addressable entity from global state.
    Success {
        /// An addressable entity.
        entry_point: EntryPointValue,
    },
    /// Failure result.
    Failure(TrackingCopyError),
}

/// Represents a request to check entry point existence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryPointExistsRequest {
    state_hash: Digest,
    entry_point_name: String,
    contract_hash: HashAddr,
}

impl EntryPointExistsRequest {
    /// ctor
    pub fn new(state_hash: Digest, entry_point_name: String, contract_hash: HashAddr) -> Self {
        EntryPointExistsRequest {
            state_hash,
            entry_point_name,
            contract_hash,
        }
    }

    /// Returns state root hash.
    pub fn state_hash(&self) -> Digest {
        self.state_hash
    }

    /// Returns entry_point_name.
    pub fn entry_point_name(&self) -> &str {
        &self.entry_point_name
    }

    /// Returns contract_hash.
    pub fn contract_hash(&self) -> HashAddr {
        self.contract_hash
    }
}

/// Represents a result of `entry_point_exists` request.
#[derive(Debug)]
pub enum EntryPointExistsResult {
    /// Invalid state root hash.
    RootNotFound,
    /// Value not found.
    ValueNotFound(String),
    /// This variant will be returned if the entry point was found.
    Success,
    /// Failure result.
    Failure(TrackingCopyError),
}

impl EntryPointExistsResult {
    /// Returns `true` if the result is `Success`.
    pub fn is_some(self) -> bool {
        matches!(self, Self::Success { .. })
    }
}
