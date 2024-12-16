use crate::{
    bytesrepr::{self, Bytes, FromBytes, ToBytes, U8_SERIALIZED_LENGTH},
    CLTyped, CLValueError, RuntimeArgs,
};
use alloc::{string::String, vec::Vec};
#[cfg(feature = "datasize")]
use datasize::DataSize;
#[cfg(feature = "json-schema")]
use schemars::JsonSchema;
#[cfg(any(feature = "std", test))]
use serde::{Deserialize, Serialize};

/// The arguments of a transaction, which can be either a named set of runtime arguments or a
/// chunked bytes.
#[derive(Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug)]
#[cfg_attr(
    any(feature = "std", test),
    derive(Serialize, Deserialize),
    serde(deny_unknown_fields)
)]
#[cfg_attr(feature = "datasize", derive(DataSize))]
#[cfg_attr(
    feature = "json-schema",
    derive(JsonSchema),
    schemars(description = "Body of a `TransactionArgs`.")
)]
pub enum TransactionArgs {
    /// Named runtime arguments.
    Named(RuntimeArgs),
    /// Bytesrepr bytes.
    Bytesrepr(Bytes),
}

impl TransactionArgs {
    /// Returns `RuntimeArgs` if the transaction arguments are named.
    pub fn as_named(&self) -> Option<&RuntimeArgs> {
        match self {
            TransactionArgs::Named(args) => Some(args),
            TransactionArgs::Bytesrepr(_) => None,
        }
    }

    /// Returns `RuntimeArgs` if the transaction arguments are mnamed.
    pub fn into_named(self) -> Option<RuntimeArgs> {
        match self {
            TransactionArgs::Named(args) => Some(args),
            TransactionArgs::Bytesrepr(_) => None,
        }
    }

    /// Returns `Bytes` if the transaction arguments are chunked.
    pub fn into_bytesrepr(self) -> Option<Bytes> {
        match self {
            TransactionArgs::Named(_) => None,
            TransactionArgs::Bytesrepr(bytes) => Some(bytes),
        }
    }

    /// Returns `Bytes` if the transaction arguments are bytes.
    pub fn as_bytesrepr(&self) -> Option<&Bytes> {
        match self {
            TransactionArgs::Named(_) => None,
            TransactionArgs::Bytesrepr(bytes) => Some(bytes),
        }
    }

    /// Inserts a key-value pair into the named runtime arguments.
    pub fn insert<K, V>(&mut self, key: K, value: V) -> Result<(), CLValueError>
    where
        K: Into<String>,
        V: CLTyped + ToBytes,
    {
        match self {
            TransactionArgs::Named(args) => {
                args.insert(key, value)?;
                Ok(())
            }
            TransactionArgs::Bytesrepr(_) => {
                Err(CLValueError::Serialization(bytesrepr::Error::Formatting))
            }
        }
    }

    /// Returns `true` if the transaction args is [`Named`].
    ///
    /// [`Named`]: TransactionArgs::Named
    #[must_use]
    pub fn is_named(&self) -> bool {
        matches!(self, Self::Named(..))
    }

    /// Returns `true` if the transaction args is [`Bytesrepr`].
    ///
    /// [`Bytesrepr`]: TransactionArgs::Bytesrepr
    #[must_use]
    pub fn is_bytesrepr(&self) -> bool {
        matches!(self, Self::Bytesrepr(..))
    }
}

impl FromBytes for TransactionArgs {
    fn from_bytes(bytes: &[u8]) -> Result<(Self, &[u8]), bytesrepr::Error> {
        let (tag, remainder) = u8::from_bytes(bytes)?;
        match tag {
            0 => {
                let (args, remainder) = RuntimeArgs::from_bytes(remainder)?;
                Ok((TransactionArgs::Named(args), remainder))
            }
            1 => {
                let (bytes, remainder) = Bytes::from_bytes(remainder)?;
                Ok((TransactionArgs::Bytesrepr(bytes), remainder))
            }
            _ => Err(bytesrepr::Error::Formatting),
        }
    }
}

impl ToBytes for TransactionArgs {
    fn to_bytes(&self) -> Result<Vec<u8>, bytesrepr::Error> {
        let mut buffer = bytesrepr::allocate_buffer(self)?;
        self.write_bytes(&mut buffer)?;
        Ok(buffer)
    }

    fn serialized_length(&self) -> usize {
        match self {
            TransactionArgs::Named(args) => args.serialized_length() + U8_SERIALIZED_LENGTH,
            TransactionArgs::Bytesrepr(bytes) => bytes.serialized_length() + U8_SERIALIZED_LENGTH,
        }
    }

    fn write_bytes(&self, writer: &mut Vec<u8>) -> Result<(), bytesrepr::Error> {
        match self {
            TransactionArgs::Named(args) => {
                writer.push(0);
                args.write_bytes(writer)
            }
            TransactionArgs::Bytesrepr(bytes) => {
                writer.push(1);
                bytes.write_bytes(writer)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use crate::{bytesrepr, gens::transaction_args_arb};

    proptest! {
        #[test]
        fn serialization_roundtrip(args in transaction_args_arb()) {
            bytesrepr::test_serialization_roundtrip(&args);
        }
    }
}
