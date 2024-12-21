use core::{
    array::TryFromSliceError,
    fmt::{self, Display, Formatter},
};

// This error type is not intended to be used by third party crates.
#[doc(hidden)]
#[derive(Debug, Eq, PartialEq)]
pub struct TryFromIntError(pub ());

/// Error returned when decoding an `AccountHash` from a formatted string.
#[derive(Debug)]
#[non_exhaustive]
pub enum FromAccountHashStrError {
    /// The prefix is invalid.
    InvalidPrefix,
    /// The hash is not valid hex.
    Hex(base16::DecodeError),
    /// The hash is the wrong length.
    Hash(TryFromSliceError),
}

impl From<base16::DecodeError> for FromAccountHashStrError {
    fn from(error: base16::DecodeError) -> Self {
        FromAccountHashStrError::Hex(error)
    }
}

impl From<TryFromSliceError> for FromAccountHashStrError {
    fn from(error: TryFromSliceError) -> Self {
        FromAccountHashStrError::Hash(error)
    }
}

impl Display for FromAccountHashStrError {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            FromAccountHashStrError::InvalidPrefix => write!(f, "prefix is not 'account-hash-'"),
            FromAccountHashStrError::Hex(error) => {
                write!(f, "failed to decode address portion from hex: {}", error)
            }
            FromAccountHashStrError::Hash(error) => {
                write!(f, "address portion is wrong length: {}", error)
            }
        }
    }
}

/// Associated error type of `TryFrom<&[u8]>` for [`AccountHash`](super::AccountHash).
#[derive(Debug)]
pub struct TryFromSliceForAccountHashError(());
