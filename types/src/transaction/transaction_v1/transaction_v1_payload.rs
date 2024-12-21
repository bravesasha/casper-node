use core::fmt::{self, Debug, Display, Formatter};

use super::{errors_v1::FieldDeserializationError, PricingMode};
use crate::{
    bytesrepr::{
        Bytes,
        Error::{self, Formatting},
        FromBytes, ToBytes,
    },
    transaction::serialization::{
        CalltableSerializationEnvelope, CalltableSerializationEnvelopeBuilder,
    },
    DisplayIter, InitiatorAddr, TimeDiff, Timestamp,
};
#[cfg(any(feature = "std", test))]
use crate::{TransactionArgs, TransactionEntryPoint, TransactionScheduling, TransactionTarget};
use alloc::{collections::BTreeMap, string::String, vec::Vec};
#[cfg(feature = "datasize")]
use datasize::DataSize;
#[cfg(feature = "json-schema")]
use schemars::JsonSchema;
#[cfg(any(feature = "std", test))]
use serde::{de::DeserializeOwned, Deserialize, Serialize};
#[cfg(any(feature = "std", test))]
use serde_json::Value;
#[cfg(any(feature = "std", test))]
use thiserror::Error;

const INITIATOR_ADDR_FIELD_INDEX: u16 = 0;
const TIMESTAMP_FIELD_INDEX: u16 = 1;
const TTL_FIELD_INDEX: u16 = 2;
const CHAIN_NAME_FIELD_INDEX: u16 = 3;
const PRICING_MODE_FIELD_INDEX: u16 = 4;
const FIELDS_FIELD_INDEX: u16 = 5;

const ARGS_MAP_KEY: u16 = 0;
const TARGET_MAP_KEY: u16 = 1;
const ENTRY_POINT_MAP_KEY: u16 = 2;
const SCHEDULING_MAP_KEY: u16 = 3;
#[cfg(any(feature = "std", test))]
const ARGS_MAP_HUMAN_READABLE_KEY: &str = "args";
#[cfg(any(feature = "std", test))]
const TARGET_MAP_HUMAN_READABLE_KEY: &str = "target";
#[cfg(any(feature = "std", test))]
const ENTRY_POINT_MAP_HUMAN_READABLE_KEY: &str = "entry_point";
#[cfg(any(feature = "std", test))]
const SCHEDULING_MAP_HUMAN_READABLE_KEY: &str = "scheduling";

const EXPECTED_FIELD_KEYS: [u16; 4] = [
    ARGS_MAP_KEY,
    TARGET_MAP_KEY,
    ENTRY_POINT_MAP_KEY,
    SCHEDULING_MAP_KEY,
];

/// Structure aggregating internal data of V1 transaction.
#[derive(Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug)]
#[cfg_attr(feature = "datasize", derive(DataSize))]
#[cfg_attr(
    any(feature = "std", test),
    derive(Serialize, Deserialize),
    serde(deny_unknown_fields)
)]
#[cfg_attr(
    feature = "json-schema",
    derive(JsonSchema),
    schemars(with = "TransactionV1PayloadJson")
)]
pub struct TransactionV1Payload {
    initiator_addr: InitiatorAddr,
    timestamp: Timestamp,
    ttl: TimeDiff,
    chain_name: String,
    pricing_mode: PricingMode,
    fields: BTreeMap<u16, Bytes>,
}

impl TransactionV1Payload {
    // ctor
    pub fn new(
        chain_name: String,
        timestamp: Timestamp,
        ttl: TimeDiff,
        pricing_mode: PricingMode,
        initiator_addr: InitiatorAddr,
        fields: BTreeMap<u16, Bytes>,
    ) -> TransactionV1Payload {
        TransactionV1Payload {
            chain_name,
            timestamp,
            ttl,
            pricing_mode,
            initiator_addr,
            fields,
        }
    }

    fn serialized_field_lengths(&self) -> Vec<usize> {
        vec![
            self.initiator_addr.serialized_length(),
            self.timestamp.serialized_length(),
            self.ttl.serialized_length(),
            self.chain_name.serialized_length(),
            self.pricing_mode.serialized_length(),
            self.fields.serialized_length(),
        ]
    }

    /// Returns the chain name of the transaction.
    pub fn chain_name(&self) -> &str {
        &self.chain_name
    }

    /// Returns the timestamp of the transaction.
    pub fn timestamp(&self) -> Timestamp {
        self.timestamp
    }

    /// Returns the time-to-live of the transaction.
    pub fn ttl(&self) -> TimeDiff {
        self.ttl
    }

    /// Returns the pricing mode of the transaction.
    pub fn pricing_mode(&self) -> &PricingMode {
        &self.pricing_mode
    }

    /// Returns the initiator address of the transaction.
    pub fn initiator_addr(&self) -> &InitiatorAddr {
        &self.initiator_addr
    }

    /// Returns the fields of the transaction.
    pub fn fields(&self) -> &BTreeMap<u16, Bytes> {
        &self.fields
    }

    /// Returns the timestamp of when the transaction expires, i.e. `self.timestamp + self.ttl`.
    pub fn expires(&self) -> Timestamp {
        self.timestamp.saturating_add(self.ttl)
    }

    /// Returns `true` if the transaction has expired.
    pub fn expired(&self, current_instant: Timestamp) -> bool {
        self.expires() < current_instant
    }

    /// Fetches field from the amorphic `field` map and attempts to deserialize it into a type `T`.
    /// The deserialization is done using the `FromBytes` trait.
    pub fn deserialize_field<T: FromBytes>(
        &self,
        index: u16,
    ) -> Result<T, FieldDeserializationError> {
        let field = self
            .fields
            .get(&index)
            .ok_or(FieldDeserializationError::IndexNotExists { index })?;
        let (value, remainder) = T::from_bytes(field)
            .map_err(|error| FieldDeserializationError::FromBytesError { index, error })?;
        if !remainder.is_empty() {
            return Err(FieldDeserializationError::LingeringBytesInField { index });
        }
        Ok(value)
    }

    /// Helper method to return size of `fields`.
    pub fn number_of_fields(&self) -> usize {
        self.fields.len()
    }

    /// Makes transaction payload invalid.
    #[cfg(any(all(feature = "std", feature = "testing"), test))]
    pub fn invalidate(&mut self) {
        self.chain_name.clear();
    }
}

#[cfg(any(feature = "std", test))]
impl TryFrom<TransactionV1PayloadJson> for TransactionV1Payload {
    type Error = TransactionV1PayloadJsonError;
    fn try_from(transaction_v1_json: TransactionV1PayloadJson) -> Result<Self, Self::Error> {
        Ok(TransactionV1Payload {
            initiator_addr: transaction_v1_json.initiator_addr,
            timestamp: transaction_v1_json.timestamp,
            ttl: transaction_v1_json.ttl,
            chain_name: transaction_v1_json.chain_name,
            pricing_mode: transaction_v1_json.pricing_mode,
            fields: from_human_readable_fields(&transaction_v1_json.fields)?,
        })
    }
}

#[cfg(any(feature = "std", test))]
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(
    feature = "json-schema",
    derive(JsonSchema),
    schemars(
        description = "Internal payload of the transaction. The actual data over which the signing is done.",
        rename = "TransactionV1Payload",
    )
)]
pub(super) struct TransactionV1PayloadJson {
    initiator_addr: InitiatorAddr,
    timestamp: Timestamp,
    ttl: TimeDiff,
    chain_name: String,
    pricing_mode: PricingMode,
    fields: BTreeMap<String, Value>,
}

#[cfg(any(feature = "std", test))]
#[derive(Error, Debug)]

pub(super) enum TransactionV1PayloadJsonError {
    #[error("{0}")]
    FailedToMap(String),
}

#[cfg(any(feature = "std", test))]
impl TryFrom<TransactionV1Payload> for TransactionV1PayloadJson {
    type Error = TransactionV1PayloadJsonError;

    fn try_from(value: TransactionV1Payload) -> Result<Self, Self::Error> {
        Ok(TransactionV1PayloadJson {
            initiator_addr: value.initiator_addr,
            timestamp: value.timestamp,
            ttl: value.ttl,
            chain_name: value.chain_name,
            pricing_mode: value.pricing_mode,
            fields: to_human_readable_fields(&value.fields)?,
        })
    }
}

#[cfg(any(feature = "std", test))]
fn from_human_readable_fields(
    fields: &BTreeMap<String, Value>,
) -> Result<BTreeMap<u16, Bytes>, TransactionV1PayloadJsonError> {
    let number_of_expected_fields = EXPECTED_FIELD_KEYS.len();
    if fields.len() != number_of_expected_fields {
        return Err(TransactionV1PayloadJsonError::FailedToMap(format!(
            "Expected exactly {} fields",
            number_of_expected_fields
        )));
    }
    let args_bytes = to_bytesrepr::<TransactionArgs>(fields, ARGS_MAP_HUMAN_READABLE_KEY)?;
    let target_bytes = to_bytesrepr::<TransactionTarget>(fields, TARGET_MAP_HUMAN_READABLE_KEY)?;
    let entry_point_bytes =
        to_bytesrepr::<TransactionEntryPoint>(fields, ENTRY_POINT_MAP_HUMAN_READABLE_KEY)?;
    let schedule_bytes =
        to_bytesrepr::<TransactionScheduling>(fields, SCHEDULING_MAP_HUMAN_READABLE_KEY)?;
    Ok(BTreeMap::from_iter(vec![
        (ARGS_MAP_KEY, args_bytes),
        (TARGET_MAP_KEY, target_bytes),
        (ENTRY_POINT_MAP_KEY, entry_point_bytes),
        (SCHEDULING_MAP_KEY, schedule_bytes),
    ]))
}

#[cfg(any(feature = "std", test))]
fn to_human_readable_fields(
    fields: &BTreeMap<u16, Bytes>,
) -> Result<BTreeMap<String, Value>, TransactionV1PayloadJsonError> {
    let args_value =
        extract_and_deserialize_field::<TransactionArgs>(fields, ARGS_MAP_KEY, "args")?;
    let target_value =
        extract_and_deserialize_field::<TransactionTarget>(fields, TARGET_MAP_KEY, "target")?;
    let entry_point_value = extract_and_deserialize_field::<TransactionEntryPoint>(
        fields,
        ENTRY_POINT_MAP_KEY,
        "entry_point",
    )?;
    let scheduling_value = extract_and_deserialize_field::<TransactionScheduling>(
        fields,
        SCHEDULING_MAP_KEY,
        "scheduling",
    )?;

    Ok(BTreeMap::from_iter(vec![
        (ARGS_MAP_HUMAN_READABLE_KEY.to_string(), args_value),
        (TARGET_MAP_HUMAN_READABLE_KEY.to_string(), target_value),
        (
            ENTRY_POINT_MAP_HUMAN_READABLE_KEY.to_string(),
            entry_point_value,
        ),
        (
            SCHEDULING_MAP_HUMAN_READABLE_KEY.to_string(),
            scheduling_value,
        ),
    ]))
}

#[cfg(any(feature = "std", test))]
fn to_bytesrepr<T: ToBytes + DeserializeOwned>(
    fields: &BTreeMap<String, Value>,
    field_name: &str,
) -> Result<Bytes, TransactionV1PayloadJsonError> {
    let value_json = fields
        .get(field_name)
        .ok_or(TransactionV1PayloadJsonError::FailedToMap(format!(
            "Could not find {field_name} field"
        )))?;
    let deserialized = serde_json::from_value::<T>(value_json.clone())
        .map_err(|e| TransactionV1PayloadJsonError::FailedToMap(format!("{:?}", e)))?;
    deserialized
        .to_bytes()
        .map(|bytes| bytes.into())
        .map_err(|e| TransactionV1PayloadJsonError::FailedToMap(format!("{:?}", e)))
}

#[cfg(any(feature = "std", test))]
fn extract_and_deserialize_field<T: FromBytes + Serialize>(
    fields: &BTreeMap<u16, Bytes>,
    key: u16,
    field_name: &str,
) -> Result<Value, TransactionV1PayloadJsonError> {
    let value_bytes = fields
        .get(&key)
        .ok_or(TransactionV1PayloadJsonError::FailedToMap(format!(
            "Could not find {field_name} field"
        )))?;
    let (from_bytes, remainder) = T::from_bytes(value_bytes)
        .map_err(|e| TransactionV1PayloadJsonError::FailedToMap(format!("{:?}", e)))?;
    if !remainder.is_empty() {
        return Err(TransactionV1PayloadJsonError::FailedToMap(format!(
            "Unexpexcted bytes in {field_name} field"
        )));
    }
    let value = serde_json::to_value(from_bytes)
        .map_err(|e| TransactionV1PayloadJsonError::FailedToMap(format!("{:?}", e)))?;
    Ok(value)
}

impl ToBytes for TransactionV1Payload {
    fn to_bytes(&self) -> Result<Vec<u8>, crate::bytesrepr::Error> {
        let expected_payload_sizes = self.serialized_field_lengths();
        CalltableSerializationEnvelopeBuilder::new(expected_payload_sizes)?
            .add_field(INITIATOR_ADDR_FIELD_INDEX, &self.initiator_addr)?
            .add_field(TIMESTAMP_FIELD_INDEX, &self.timestamp)?
            .add_field(TTL_FIELD_INDEX, &self.ttl)?
            .add_field(CHAIN_NAME_FIELD_INDEX, &self.chain_name)?
            .add_field(PRICING_MODE_FIELD_INDEX, &self.pricing_mode)?
            .add_field(FIELDS_FIELD_INDEX, &self.fields)?
            .binary_payload_bytes()
    }

    fn serialized_length(&self) -> usize {
        CalltableSerializationEnvelope::estimate_size(self.serialized_field_lengths())
    }
}

impl FromBytes for TransactionV1Payload {
    fn from_bytes(bytes: &[u8]) -> Result<(Self, &[u8]), Error> {
        let (binary_payload, remainder) = CalltableSerializationEnvelope::from_bytes(6, bytes)?;
        let window = binary_payload.start_consuming()?.ok_or(Formatting)?;

        window.verify_index(INITIATOR_ADDR_FIELD_INDEX)?;
        let (initiator_addr, window) = window.deserialize_and_maybe_next::<InitiatorAddr>()?;
        let window = window.ok_or(Formatting)?;
        window.verify_index(TIMESTAMP_FIELD_INDEX)?;
        let (timestamp, window) = window.deserialize_and_maybe_next::<Timestamp>()?;
        let window = window.ok_or(Formatting)?;
        window.verify_index(TTL_FIELD_INDEX)?;
        let (ttl, window) = window.deserialize_and_maybe_next::<TimeDiff>()?;
        let window = window.ok_or(Formatting)?;
        window.verify_index(CHAIN_NAME_FIELD_INDEX)?;
        let (chain_name, window) = window.deserialize_and_maybe_next::<String>()?;
        let window = window.ok_or(Formatting)?;
        window.verify_index(PRICING_MODE_FIELD_INDEX)?;
        let (pricing_mode, window) = window.deserialize_and_maybe_next::<PricingMode>()?;
        let window = window.ok_or(Formatting)?;
        window.verify_index(FIELDS_FIELD_INDEX)?;
        let (fields_as_vec, window) = window.deserialize_and_maybe_next::<Vec<(u16, Bytes)>>()?;
        let fields = build_map(fields_as_vec)?;
        if window.is_some() {
            return Err(Formatting);
        }
        if fields.len() != EXPECTED_FIELD_KEYS.len()
            || EXPECTED_FIELD_KEYS
                .iter()
                .any(|expected_key| !fields.contains_key(expected_key))
        {
            return Err(Formatting);
        }
        let from_bytes = TransactionV1Payload {
            chain_name,
            timestamp,
            ttl,
            pricing_mode,
            initiator_addr,
            fields,
        };

        Ok((from_bytes, remainder))
    }
}

// We need to make sure that the bytes of the `fields` field are serialized in the correct order.
// A BTreeMap is serialized the same as Vec<(K, V)> and it actually, on deserialization, doesn't
// check if the keys are in ascending order. We need to make sure that the incoming transaction
// payload is serialized in a strict way, otherwise we would have trouble with verifying the
// signature(s).
fn build_map(fields_as_vec: Vec<(u16, Bytes)>) -> Result<BTreeMap<u16, Bytes>, Error> {
    let mut ret = BTreeMap::new();
    let mut max_idx: i32 = -1;
    for (key, value) in fields_as_vec {
        let key_signed = key as i32;
        if key_signed <= max_idx {
            return Err(Formatting);
        }
        max_idx = key_signed;
        ret.insert(key, value);
    }

    Ok(ret)
}

impl Display for TransactionV1Payload {
    fn fmt(&self, formatter: &mut Formatter) -> fmt::Result {
        write!(
            formatter,
            "transaction-v1-payload[{}, {}, {}, {}, {}, fields: {}]",
            self.chain_name,
            self.timestamp,
            self.ttl,
            self.pricing_mode,
            self.initiator_addr,
            DisplayIter::new(self.fields.keys())
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        testing::TestRng, RuntimeArgs, TransactionEntryPoint, TransactionScheduling,
        TransactionTarget,
    };
    use std::collections::BTreeMap;

    #[test]
    fn reserialize_should_work_with_ascending_ids() {
        let input = vec![
            (0, Bytes::from(vec![1])),
            (1, Bytes::from(vec![2])),
            (4, Bytes::from(vec![3])),
        ];
        let map = build_map(input).expect("Should not fail");
        assert_eq!(
            map,
            BTreeMap::from_iter(vec![
                (0, Bytes::from(vec![1])),
                (1, Bytes::from(vec![2])),
                (4, Bytes::from(vec![3]))
            ])
        );
    }

    #[test]
    fn reserialize_should_fail_when_ids_not_unique() {
        let input = vec![
            (0, Bytes::from(vec![1])),
            (0, Bytes::from(vec![2])),
            (4, Bytes::from(vec![3])),
        ];
        let map_ret = build_map(input);
        assert!(map_ret.is_err());
    }

    #[test]
    fn reserialize_should_fail_when_ids_not_ascending() {
        let input = vec![
            (0, Bytes::from(vec![1])),
            (2, Bytes::from(vec![2])),
            (1, Bytes::from(vec![3])),
        ];
        assert!(build_map(input).is_err());
        let input = vec![
            (0, Bytes::from(vec![1])),
            (2, Bytes::from(vec![2])),
            (0, Bytes::from(vec![3])),
        ];
        assert!(build_map(input).is_err());
        let input = vec![
            (0, Bytes::from(vec![1])),
            (1, Bytes::from(vec![2])),
            (2, Bytes::from(vec![3])),
            (3, Bytes::from(vec![4])),
            (2, Bytes::from(vec![5])),
        ];
        assert!(build_map(input).is_err());
    }

    #[test]
    fn should_fail_if_deserialized_payload_has_too_many_fields() {
        let rng = &mut TestRng::new();
        let (
            args,
            target,
            entry_point,
            scheduling,
            initiator_addr,
            timestamp,
            ttl,
            chain_name,
            pricing_mode,
        ) = random_payload_data(rng);
        let mut fields = BTreeMap::new();
        fields.insert(ARGS_MAP_KEY, args.to_bytes().unwrap().into());
        fields.insert(TARGET_MAP_KEY, target.to_bytes().unwrap().into());
        fields.insert(ENTRY_POINT_MAP_KEY, entry_point.to_bytes().unwrap().into());
        fields.insert(SCHEDULING_MAP_KEY, scheduling.to_bytes().unwrap().into());
        fields.insert(4, 111_u64.to_bytes().unwrap().into());

        let bytes = TransactionV1Payload::new(
            chain_name,
            timestamp,
            ttl,
            pricing_mode,
            initiator_addr,
            fields,
        )
        .to_bytes()
        .unwrap();
        let result = TransactionV1Payload::from_bytes(&bytes);
        assert!(result.is_err());
    }

    #[test]
    fn should_fail_if_deserialized_payload_has_unrecognized_fields() {
        let rng = &mut TestRng::new();
        let (
            args,
            target,
            entry_point,
            scheduling,
            initiator_addr,
            timestamp,
            ttl,
            chain_name,
            pricing_mode,
        ) = random_payload_data(rng);
        let mut fields = BTreeMap::new();
        fields.insert(ARGS_MAP_KEY, args.to_bytes().unwrap().into());
        fields.insert(TARGET_MAP_KEY, target.to_bytes().unwrap().into());
        fields.insert(100, entry_point.to_bytes().unwrap().into());
        fields.insert(SCHEDULING_MAP_KEY, scheduling.to_bytes().unwrap().into());

        let bytes = TransactionV1Payload::new(
            chain_name,
            timestamp,
            ttl,
            pricing_mode,
            initiator_addr,
            fields,
        )
        .to_bytes()
        .unwrap();
        let result = TransactionV1Payload::from_bytes(&bytes);
        assert!(result.is_err());
    }

    #[test]
    fn should_fail_if_serialized_payoad_has_fields_out_of_order() {
        let rng = &mut TestRng::new();
        let (
            args,
            target,
            entry_point,
            scheduling,
            initiator_addr,
            timestamp,
            ttl,
            chain_name,
            pricing_mode,
        ) = random_payload_data(rng);
        let fields: Vec<(u16, Bytes)> = vec![
            (SCHEDULING_MAP_KEY, scheduling.to_bytes().unwrap().into()),
            (TARGET_MAP_KEY, target.to_bytes().unwrap().into()),
            (ENTRY_POINT_MAP_KEY, entry_point.to_bytes().unwrap().into()),
            (ARGS_MAP_KEY, args.to_bytes().unwrap().into()),
        ];

        let expected_payload_sizes = vec![
            initiator_addr.serialized_length(),
            timestamp.serialized_length(),
            ttl.serialized_length(),
            chain_name.serialized_length(),
            pricing_mode.serialized_length(),
            fields.serialized_length(),
        ];

        let bytes = CalltableSerializationEnvelopeBuilder::new(expected_payload_sizes)
            .unwrap()
            .add_field(INITIATOR_ADDR_FIELD_INDEX, &initiator_addr)
            .unwrap()
            .add_field(TIMESTAMP_FIELD_INDEX, &timestamp)
            .unwrap()
            .add_field(TTL_FIELD_INDEX, &ttl)
            .unwrap()
            .add_field(CHAIN_NAME_FIELD_INDEX, &chain_name)
            .unwrap()
            .add_field(PRICING_MODE_FIELD_INDEX, &pricing_mode)
            .unwrap()
            .add_field(FIELDS_FIELD_INDEX, &fields)
            .unwrap()
            .binary_payload_bytes()
            .unwrap();
        let payload_res = TransactionV1Payload::from_bytes(&bytes);
        assert!(payload_res.is_err());
    }

    fn random_payload_data(
        rng: &mut TestRng,
    ) -> (
        RuntimeArgs,
        TransactionTarget,
        TransactionEntryPoint,
        TransactionScheduling,
        InitiatorAddr,
        Timestamp,
        TimeDiff,
        String,
        PricingMode,
    ) {
        let args = RuntimeArgs::random(rng);
        let target = TransactionTarget::random(rng);
        let entry_point = TransactionEntryPoint::random(rng);
        let scheduling = TransactionScheduling::random(rng);
        let initiator_addr = InitiatorAddr::random(rng);
        let timestamp = Timestamp::now();
        let ttl = TimeDiff::from_millis(1000);
        let chain_name = "chain-name".to_string();
        let pricing_mode = PricingMode::random(rng);
        (
            args,
            target,
            entry_point,
            scheduling,
            initiator_addr,
            timestamp,
            ttl,
            chain_name,
            pricing_mode,
        )
    }
}
