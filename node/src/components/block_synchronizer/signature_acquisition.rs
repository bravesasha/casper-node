use std::collections::BTreeMap;

use datasize::DataSize;
use itertools::Itertools;

use crate::types::FinalitySignature;
use casper_types::PublicKey;

#[derive(Clone, PartialEq, Eq, DataSize, Debug)]
pub(super) enum SignatureState {
    Vacant,
    Signature(Box<FinalitySignature>),
}

#[derive(Clone, PartialEq, Eq, DataSize, Debug)]
pub(crate) struct SignatureAcquisition {
    inner: BTreeMap<PublicKey, SignatureState>,
}

impl SignatureAcquisition {
    pub(super) fn new(validators: Vec<PublicKey>) -> Self {
        let mut inner = BTreeMap::new();
        validators
            .into_iter()
            .map(|validator| inner.insert(validator, SignatureState::Vacant));
        SignatureAcquisition { inner }
    }

    // Returns `true` if new signature was registered.
    pub(super) fn apply_signature(&mut self, finality_signature: FinalitySignature) -> bool {
        self.inner
            .insert(
                finality_signature.public_key.clone(),
                SignatureState::Signature(Box::new(finality_signature)),
            )
            .is_none()
    }

    pub(super) fn needing_signatures(&self) -> Vec<PublicKey> {
        self.inner
            .iter()
            .filter(|(k, v)| **v == SignatureState::Vacant)
            .map(|(k, _)| k.clone())
            .collect_vec()
    }

    pub(super) fn have_signatures(&self) -> impl Iterator<Item = &PublicKey> {
        self.inner.iter().filter_map(|(k, v)| match v {
            SignatureState::Vacant => None,
            SignatureState::Signature(finality_signature) => Some(k),
        })
    }

    pub(super) fn is_non_vacant(&self) -> bool {
        self.inner
            .iter()
            .any(|(_public_key, signature)| *signature != SignatureState::Vacant)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}