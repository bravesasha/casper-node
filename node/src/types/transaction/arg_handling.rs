//! Collection of helper functions and structures to reason about amorphic RuntimeArgs.
use core::marker::PhantomData;

use casper_types::{
    account::AccountHash,
    bytesrepr::FromBytes,
    system::auction::{DelegatorKind, Reservation, ARG_VALIDATOR},
    CLType, CLTyped, CLValue, CLValueError, InvalidTransactionV1, PublicKey, RuntimeArgs,
    TransactionArgs, URef, U512,
};
#[cfg(test)]
use casper_types::{bytesrepr::ToBytes, TransferTarget};
use tracing::debug;

const TRANSFER_ARG_AMOUNT: RequiredArg<U512> = RequiredArg::new("amount");
const TRANSFER_ARG_SOURCE: OptionalArg<URef> = OptionalArg::new("source");
const TRANSFER_ARG_TARGET: &str = "target";
// "id" for legacy reasons, if the argument is passed it is [Option]
const TRANSFER_ARG_ID: OptionalArg<Option<u64>> = OptionalArg::new("id");

const ADD_BID_ARG_PUBLIC_KEY: RequiredArg<PublicKey> = RequiredArg::new("public_key");
const ADD_BID_ARG_DELEGATION_RATE: RequiredArg<u8> = RequiredArg::new("delegation_rate");
const ADD_BID_ARG_AMOUNT: RequiredArg<U512> = RequiredArg::new("amount");
const ADD_BID_ARG_MINIMUM_DELEGATION_AMOUNT: OptionalArg<u64> =
    OptionalArg::new("minimum_delegation_amount");
const ADD_BID_ARG_MAXIMUM_DELEGATION_AMOUNT: OptionalArg<u64> =
    OptionalArg::new("maximum_delegation_amount");
const ADD_BID_ARG_RESERVED_SLOTS: OptionalArg<u32> = OptionalArg::new("reserved_slots");

const WITHDRAW_BID_ARG_PUBLIC_KEY: RequiredArg<PublicKey> = RequiredArg::new("public_key");
const WITHDRAW_BID_ARG_AMOUNT: RequiredArg<U512> = RequiredArg::new("amount");

const DELEGATE_ARG_DELEGATOR: RequiredArg<PublicKey> = RequiredArg::new("delegator");
const DELEGATE_ARG_VALIDATOR: RequiredArg<PublicKey> = RequiredArg::new("validator");
const DELEGATE_ARG_AMOUNT: RequiredArg<U512> = RequiredArg::new("amount");

const UNDELEGATE_ARG_DELEGATOR: RequiredArg<PublicKey> = RequiredArg::new("delegator");
const UNDELEGATE_ARG_VALIDATOR: RequiredArg<PublicKey> = RequiredArg::new("validator");
const UNDELEGATE_ARG_AMOUNT: RequiredArg<U512> = RequiredArg::new("amount");

const REDELEGATE_ARG_DELEGATOR: RequiredArg<PublicKey> = RequiredArg::new("delegator");
const REDELEGATE_ARG_VALIDATOR: RequiredArg<PublicKey> = RequiredArg::new("validator");
const REDELEGATE_ARG_AMOUNT: RequiredArg<U512> = RequiredArg::new("amount");
const REDELEGATE_ARG_NEW_VALIDATOR: RequiredArg<PublicKey> = RequiredArg::new("new_validator");

const ACTIVATE_BID_ARG_VALIDATOR: RequiredArg<PublicKey> = RequiredArg::new(ARG_VALIDATOR);

const CHANGE_BID_PUBLIC_KEY_ARG_PUBLIC_KEY: RequiredArg<PublicKey> = RequiredArg::new("public_key");
const CHANGE_BID_PUBLIC_KEY_ARG_NEW_PUBLIC_KEY: RequiredArg<PublicKey> =
    RequiredArg::new("new_public_key");

const ADD_RESERVATIONS_ARG_RESERVATIONS: RequiredArg<Vec<Reservation>> =
    RequiredArg::new("reservations");

const CANCEL_RESERVATIONS_ARG_VALIDATOR: RequiredArg<PublicKey> = RequiredArg::new("validator");
const CANCEL_RESERVATIONS_ARG_DELEGATORS: RequiredArg<Vec<DelegatorKind>> =
    RequiredArg::new("delegators");

struct RequiredArg<T> {
    name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> RequiredArg<T> {
    const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    fn get(&self, args: &RuntimeArgs) -> Result<T, InvalidTransactionV1>
    where
        T: CLTyped + FromBytes,
    {
        let cl_value = args.get(self.name).ok_or_else(|| {
            debug!("missing required runtime argument '{}'", self.name);
            InvalidTransactionV1::MissingArg {
                arg_name: self.name.to_string(),
            }
        })?;
        parse_cl_value(cl_value, self.name)
    }

    #[cfg(test)]
    fn insert(&self, args: &mut RuntimeArgs, value: T) -> Result<(), CLValueError>
    where
        T: CLTyped + ToBytes,
    {
        args.insert(self.name, value)
    }
}

struct OptionalArg<T> {
    name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> OptionalArg<T> {
    const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    fn get(&self, args: &RuntimeArgs) -> Result<Option<T>, InvalidTransactionV1>
    where
        T: CLTyped + FromBytes,
    {
        let cl_value = match args.get(self.name) {
            Some(value) => value,
            None => return Ok(None),
        };
        let value = parse_cl_value::<T>(cl_value, self.name)?;
        Ok(Some(value))
    }

    #[cfg(test)]
    fn insert(&self, args: &mut RuntimeArgs, value: T) -> Result<(), CLValueError>
    where
        T: CLTyped + ToBytes,
    {
        args.insert(self.name, value)
    }
}

fn parse_cl_value<T: CLTyped + FromBytes>(
    cl_value: &CLValue,
    arg_name: &str,
) -> Result<T, InvalidTransactionV1> {
    cl_value.to_t::<T>().map_err(|error| {
        let error = match error {
            CLValueError::Serialization(error) => InvalidTransactionV1::InvalidArg {
                arg_name: arg_name.to_string(),
                error,
            },
            CLValueError::Type(_) => InvalidTransactionV1::UnexpectedArgType {
                arg_name: arg_name.to_string(),
                expected: vec![T::cl_type()],
                got: cl_value.cl_type().clone(),
            },
        };
        debug!("{error}");
        error
    })
}

/// Creates a `RuntimeArgs` suitable for use in a transfer transaction.
#[cfg(test)]
pub fn new_transfer_args<A: Into<U512>, T: Into<TransferTarget>>(
    amount: A,
    maybe_source: Option<URef>,
    target: T,
    maybe_id: Option<u64>,
) -> Result<RuntimeArgs, CLValueError> {
    let mut args = RuntimeArgs::new();
    if let Some(source) = maybe_source {
        TRANSFER_ARG_SOURCE.insert(&mut args, source)?;
    }
    match target.into() {
        TransferTarget::PublicKey(public_key) => args.insert(TRANSFER_ARG_TARGET, public_key)?,
        TransferTarget::AccountHash(account_hash) => {
            args.insert(TRANSFER_ARG_TARGET, account_hash)?
        }
        TransferTarget::URef(uref) => args.insert(TRANSFER_ARG_TARGET, uref)?,
    }
    TRANSFER_ARG_AMOUNT.insert(&mut args, amount.into())?;
    if maybe_id.is_some() {
        TRANSFER_ARG_ID.insert(&mut args, maybe_id)?;
    }
    Ok(args)
}

/// Checks the given `RuntimeArgs` are suitable for use in a transfer transaction.
pub fn has_valid_transfer_args(
    args: &TransactionArgs,
    native_transfer_minimum_motes: u64,
) -> Result<(), InvalidTransactionV1> {
    let args = args
        .as_named()
        .ok_or(InvalidTransactionV1::ExpectedNamedArguments)?;

    let amount = TRANSFER_ARG_AMOUNT.get(args)?;
    if amount < U512::from(native_transfer_minimum_motes) {
        debug!(
            minimum = %native_transfer_minimum_motes,
            %amount,
            "insufficient transfer amount"
        );
        return Err(InvalidTransactionV1::InsufficientTransferAmount {
            minimum: native_transfer_minimum_motes,
            attempted: amount,
        });
    }
    let _source = TRANSFER_ARG_SOURCE.get(args)?;

    let target_cl_value = args.get(TRANSFER_ARG_TARGET).ok_or_else(|| {
        debug!("missing required runtime argument '{TRANSFER_ARG_TARGET}'");
        InvalidTransactionV1::MissingArg {
            arg_name: TRANSFER_ARG_TARGET.to_string(),
        }
    })?;
    match target_cl_value.cl_type() {
        CLType::PublicKey => {
            let _ = parse_cl_value::<PublicKey>(target_cl_value, TRANSFER_ARG_TARGET);
        }
        CLType::ByteArray(32) => {
            let _ = parse_cl_value::<AccountHash>(target_cl_value, TRANSFER_ARG_TARGET);
        }
        CLType::URef => {
            let _ = parse_cl_value::<URef>(target_cl_value, TRANSFER_ARG_TARGET);
        }
        _ => {
            debug!(
                "expected runtime argument '{TRANSFER_ARG_TARGET}' to be of type {}, {} or {},
                but is {}",
                CLType::PublicKey,
                CLType::ByteArray(32),
                CLType::URef,
                target_cl_value.cl_type()
            );
            return Err(InvalidTransactionV1::UnexpectedArgType {
                arg_name: TRANSFER_ARG_TARGET.to_string(),
                expected: vec![CLType::PublicKey, CLType::ByteArray(32), CLType::URef],
                got: target_cl_value.cl_type().clone(),
            });
        }
    }

    let _maybe_id = TRANSFER_ARG_ID.get(args)?;
    Ok(())
}

/// Creates a `RuntimeArgs` suitable for use in an add_bid transaction.
#[cfg(test)]
pub fn new_add_bid_args<A: Into<U512>>(
    public_key: PublicKey,
    delegation_rate: u8,
    amount: A,
    maybe_minimum_delegation_amount: Option<u64>,
    maybe_maximum_delegation_amount: Option<u64>,
    maybe_reserved_slots: Option<u32>,
) -> Result<RuntimeArgs, CLValueError> {
    let mut args = RuntimeArgs::new();
    ADD_BID_ARG_PUBLIC_KEY.insert(&mut args, public_key)?;
    ADD_BID_ARG_DELEGATION_RATE.insert(&mut args, delegation_rate)?;
    ADD_BID_ARG_AMOUNT.insert(&mut args, amount.into())?;
    if let Some(minimum_delegation_amount) = maybe_minimum_delegation_amount {
        ADD_BID_ARG_MINIMUM_DELEGATION_AMOUNT.insert(&mut args, minimum_delegation_amount)?;
    };
    if let Some(maximum_delegation_amount) = maybe_maximum_delegation_amount {
        ADD_BID_ARG_MAXIMUM_DELEGATION_AMOUNT.insert(&mut args, maximum_delegation_amount)?;
    };
    if let Some(reserved_slots) = maybe_reserved_slots {
        ADD_BID_ARG_RESERVED_SLOTS.insert(&mut args, reserved_slots)?;
    };
    Ok(args)
}

/// Checks the given `RuntimeArgs` are suitable for use in an add_bid transaction.
pub fn has_valid_add_bid_args(args: &TransactionArgs) -> Result<(), InvalidTransactionV1> {
    let args = args
        .as_named()
        .ok_or(InvalidTransactionV1::ExpectedNamedArguments)?;
    let _public_key = ADD_BID_ARG_PUBLIC_KEY.get(args)?;
    let _delegation_rate = ADD_BID_ARG_DELEGATION_RATE.get(args)?;
    let _amount = ADD_BID_ARG_AMOUNT.get(args)?;
    let _minimum_delegation_amount = ADD_BID_ARG_MINIMUM_DELEGATION_AMOUNT.get(args)?;
    let _maximum_delegation_amount = ADD_BID_ARG_MAXIMUM_DELEGATION_AMOUNT.get(args)?;
    let _reserved_slots = ADD_BID_ARG_RESERVED_SLOTS.get(args)?;
    Ok(())
}

/// Creates a `RuntimeArgs` suitable for use in a withdraw_bid transaction.
#[cfg(test)]
pub fn new_withdraw_bid_args<A: Into<U512>>(
    public_key: PublicKey,
    amount: A,
) -> Result<RuntimeArgs, CLValueError> {
    let mut args = RuntimeArgs::new();
    WITHDRAW_BID_ARG_PUBLIC_KEY.insert(&mut args, public_key)?;
    WITHDRAW_BID_ARG_AMOUNT.insert(&mut args, amount.into())?;
    Ok(args)
}

/// Checks the given `RuntimeArgs` are suitable for use in an withdraw_bid transaction.
pub fn has_valid_withdraw_bid_args(args: &TransactionArgs) -> Result<(), InvalidTransactionV1> {
    let args = args
        .as_named()
        .ok_or(InvalidTransactionV1::ExpectedNamedArguments)?;
    let _public_key = WITHDRAW_BID_ARG_PUBLIC_KEY.get(args)?;
    let _amount = WITHDRAW_BID_ARG_AMOUNT.get(args)?;
    Ok(())
}

/// Creates a `RuntimeArgs` suitable for use in a delegate transaction.
#[cfg(test)]
pub fn new_delegate_args<A: Into<U512>>(
    delegator: PublicKey,
    validator: PublicKey,
    amount: A,
) -> Result<RuntimeArgs, CLValueError> {
    let mut args = RuntimeArgs::new();
    DELEGATE_ARG_DELEGATOR.insert(&mut args, delegator)?;
    DELEGATE_ARG_VALIDATOR.insert(&mut args, validator)?;
    DELEGATE_ARG_AMOUNT.insert(&mut args, amount.into())?;
    Ok(args)
}

/// Checks the given `RuntimeArgs` are suitable for use in a delegate transaction.
pub fn has_valid_delegate_args(args: &TransactionArgs) -> Result<(), InvalidTransactionV1> {
    let args = args
        .as_named()
        .ok_or(InvalidTransactionV1::ExpectedNamedArguments)?;
    let _delegator = DELEGATE_ARG_DELEGATOR.get(args)?;
    let _validator = DELEGATE_ARG_VALIDATOR.get(args)?;
    let _amount = DELEGATE_ARG_AMOUNT.get(args)?;
    Ok(())
}

/// Creates a `RuntimeArgs` suitable for use in an undelegate transaction.
#[cfg(test)]
pub fn new_undelegate_args<A: Into<U512>>(
    delegator: PublicKey,
    validator: PublicKey,
    amount: A,
) -> Result<RuntimeArgs, CLValueError> {
    let mut args = RuntimeArgs::new();
    UNDELEGATE_ARG_DELEGATOR.insert(&mut args, delegator)?;
    UNDELEGATE_ARG_VALIDATOR.insert(&mut args, validator)?;
    UNDELEGATE_ARG_AMOUNT.insert(&mut args, amount.into())?;
    Ok(args)
}

/// Checks the given `RuntimeArgs` are suitable for use in an undelegate transaction.
pub fn has_valid_undelegate_args(args: &TransactionArgs) -> Result<(), InvalidTransactionV1> {
    let args = args
        .as_named()
        .ok_or(InvalidTransactionV1::ExpectedNamedArguments)?;
    let _delegator = UNDELEGATE_ARG_DELEGATOR.get(args)?;
    let _validator = UNDELEGATE_ARG_VALIDATOR.get(args)?;
    let _amount = UNDELEGATE_ARG_AMOUNT.get(args)?;
    Ok(())
}

/// Creates a `RuntimeArgs` suitable for use in a redelegate transaction.
#[cfg(test)]
pub fn new_redelegate_args<A: Into<U512>>(
    delegator: PublicKey,
    validator: PublicKey,
    amount: A,
    new_validator: PublicKey,
) -> Result<RuntimeArgs, CLValueError> {
    let mut args = RuntimeArgs::new();
    REDELEGATE_ARG_DELEGATOR.insert(&mut args, delegator)?;
    REDELEGATE_ARG_VALIDATOR.insert(&mut args, validator)?;
    REDELEGATE_ARG_AMOUNT.insert(&mut args, amount.into())?;
    REDELEGATE_ARG_NEW_VALIDATOR.insert(&mut args, new_validator)?;
    Ok(args)
}

/// Checks the given `RuntimeArgs` are suitable for use in a redelegate transaction.
pub fn has_valid_redelegate_args(args: &TransactionArgs) -> Result<(), InvalidTransactionV1> {
    let args = args
        .as_named()
        .ok_or(InvalidTransactionV1::ExpectedNamedArguments)?;
    let _delegator = REDELEGATE_ARG_DELEGATOR.get(args)?;
    let _validator = REDELEGATE_ARG_VALIDATOR.get(args)?;
    let _amount = REDELEGATE_ARG_AMOUNT.get(args)?;
    let _new_validator = REDELEGATE_ARG_NEW_VALIDATOR.get(args)?;
    Ok(())
}

/// Checks the given `RuntimeArgs` are suitable for use in an activate bid transaction.
pub fn has_valid_activate_bid_args(args: &TransactionArgs) -> Result<(), InvalidTransactionV1> {
    let args = args
        .as_named()
        .ok_or(InvalidTransactionV1::ExpectedNamedArguments)?;
    let _validator = ACTIVATE_BID_ARG_VALIDATOR.get(args)?;
    Ok(())
}

/// Creates a `RuntimeArgs` suitable for use in a change bid public key transaction.
#[cfg(test)]
pub fn new_change_bid_public_key_args(
    public_key: PublicKey,
    new_public_key: PublicKey,
) -> Result<RuntimeArgs, CLValueError> {
    let mut args = RuntimeArgs::new();
    CHANGE_BID_PUBLIC_KEY_ARG_PUBLIC_KEY.insert(&mut args, public_key)?;
    CHANGE_BID_PUBLIC_KEY_ARG_NEW_PUBLIC_KEY.insert(&mut args, new_public_key)?;
    Ok(args)
}

/// Checks the given `RuntimeArgs` are suitable for use in a change bid public key transaction.
pub fn has_valid_change_bid_public_key_args(
    args: &TransactionArgs,
) -> Result<(), InvalidTransactionV1> {
    let args = args
        .as_named()
        .ok_or(InvalidTransactionV1::ExpectedNamedArguments)?;
    let _public_key = CHANGE_BID_PUBLIC_KEY_ARG_PUBLIC_KEY.get(args)?;
    let _new_public_key = CHANGE_BID_PUBLIC_KEY_ARG_NEW_PUBLIC_KEY.get(args)?;
    Ok(())
}

/// Creates a `RuntimeArgs` suitable for use in a add resrvations transaction.
#[cfg(test)]
pub fn new_add_reservations_args(
    reservations: Vec<Reservation>,
) -> Result<RuntimeArgs, CLValueError> {
    let mut args = RuntimeArgs::new();
    ADD_RESERVATIONS_ARG_RESERVATIONS.insert(&mut args, reservations)?;
    Ok(args)
}

/// Checks the given `TransactionArgs` are suitable for use in a add reservations transaction.
pub fn has_valid_add_reservations_args(args: &TransactionArgs) -> Result<(), InvalidTransactionV1> {
    let args = args
        .as_named()
        .ok_or(InvalidTransactionV1::ExpectedNamedArguments)?;
    let _reservations = ADD_RESERVATIONS_ARG_RESERVATIONS.get(args)?;
    Ok(())
}

/// Creates a `RuntimeArgs` suitable for use in a cancel reservations transaction.
#[cfg(test)]
pub fn new_cancel_reservations_args(
    validator: PublicKey,
    delegators: Vec<DelegatorKind>,
) -> Result<RuntimeArgs, CLValueError> {
    let mut args = RuntimeArgs::new();
    CANCEL_RESERVATIONS_ARG_VALIDATOR.insert(&mut args, validator)?;
    CANCEL_RESERVATIONS_ARG_DELEGATORS.insert(&mut args, delegators)?;
    Ok(args)
}

/// Checks the given `TransactionArgs` are suitable for use in a add reservations transaction.
pub fn has_valid_cancel_reservations_args(
    args: &TransactionArgs,
) -> Result<(), InvalidTransactionV1> {
    let args = args
        .as_named()
        .ok_or(InvalidTransactionV1::ExpectedNamedArguments)?;
    let _validator = CANCEL_RESERVATIONS_ARG_VALIDATOR.get(args)?;
    let _delegators = CANCEL_RESERVATIONS_ARG_DELEGATORS.get(args)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use core::ops::Range;

    use rand::Rng;

    use super::*;
    use casper_types::{runtime_args, testing::TestRng, CLType, TransactionArgs};

    #[test]
    fn should_validate_transfer_args() {
        let rng = &mut TestRng::new();
        let min_motes = 10_u64;
        // Check random args, PublicKey target, within motes limit.
        let args = new_transfer_args(
            U512::from(rng.gen_range(min_motes..=u64::MAX)),
            rng.gen::<bool>().then(|| rng.gen()),
            PublicKey::random(rng),
            rng.gen::<bool>().then(|| rng.gen()),
        )
        .unwrap();
        has_valid_transfer_args(&TransactionArgs::Named(args), min_motes).unwrap();

        // Check random args, AccountHash target, within motes limit.
        let args = new_transfer_args(
            U512::from(rng.gen_range(min_motes..=u64::MAX)),
            rng.gen::<bool>().then(|| rng.gen()),
            rng.gen::<AccountHash>(),
            rng.gen::<bool>().then(|| rng.gen()),
        )
        .unwrap();
        has_valid_transfer_args(&TransactionArgs::Named(args), min_motes).unwrap();

        // Check random args, URef target, within motes limit.
        let args = new_transfer_args(
            U512::from(rng.gen_range(min_motes..=u64::MAX)),
            rng.gen::<bool>().then(|| rng.gen()),
            rng.gen::<URef>(),
            rng.gen::<bool>().then(|| rng.gen()),
        )
        .unwrap();
        has_valid_transfer_args(&TransactionArgs::Named(args), min_motes).unwrap();

        // Check at minimum motes limit.
        let args = new_transfer_args(
            U512::from(min_motes),
            rng.gen::<bool>().then(|| rng.gen()),
            PublicKey::random(rng),
            rng.gen::<bool>().then(|| rng.gen()),
        )
        .unwrap();
        has_valid_transfer_args(&TransactionArgs::Named(args), min_motes).unwrap();

        // Check with extra arg.
        let mut args = new_transfer_args(
            U512::from(min_motes),
            rng.gen::<bool>().then(|| rng.gen()),
            PublicKey::random(rng),
            rng.gen::<bool>().then(|| rng.gen()),
        )
        .unwrap();
        args.insert("a", 1).unwrap();
        has_valid_transfer_args(&TransactionArgs::Named(args), min_motes).unwrap();
    }

    #[test]
    fn transfer_args_with_low_amount_should_be_invalid() {
        let rng = &mut TestRng::new();
        let min_motes = 10_u64;

        let args = runtime_args! {
            TRANSFER_ARG_AMOUNT.name => U512::from(min_motes - 1),
            TRANSFER_ARG_TARGET => PublicKey::random(rng)
        };

        let expected_error = InvalidTransactionV1::InsufficientTransferAmount {
            minimum: min_motes,
            attempted: U512::from(min_motes - 1),
        };

        assert_eq!(
            has_valid_transfer_args(&TransactionArgs::Named(args), min_motes),
            Err(expected_error)
        );
    }

    #[test]
    fn transfer_args_with_missing_required_should_be_invalid() {
        let rng = &mut TestRng::new();
        let min_motes = 10_u64;

        // Missing "target".
        let args = runtime_args! {
            TRANSFER_ARG_AMOUNT.name => U512::from(min_motes),
        };
        let expected_error = InvalidTransactionV1::MissingArg {
            arg_name: TRANSFER_ARG_TARGET.to_string(),
        };
        assert_eq!(
            has_valid_transfer_args(&TransactionArgs::Named(args), min_motes),
            Err(expected_error)
        );

        // Missing "amount".
        let args = runtime_args! {
            TRANSFER_ARG_TARGET => PublicKey::random(rng)
        };
        let expected_error = InvalidTransactionV1::MissingArg {
            arg_name: TRANSFER_ARG_AMOUNT.name.to_string(),
        };
        assert_eq!(
            has_valid_transfer_args(&TransactionArgs::Named(args), min_motes),
            Err(expected_error)
        );
    }

    #[test]
    fn transfer_args_with_wrong_type_should_be_invalid() {
        let rng = &mut TestRng::new();
        let min_motes = 10_u64;

        // Wrong "target" type (a required arg).
        let args = runtime_args! {
            TRANSFER_ARG_AMOUNT.name => U512::from(min_motes),
            TRANSFER_ARG_TARGET => "wrong"
        };
        let expected_error = InvalidTransactionV1::UnexpectedArgType {
            arg_name: TRANSFER_ARG_TARGET.to_string(),
            expected: vec![CLType::PublicKey, CLType::ByteArray(32), CLType::URef],
            got: CLType::String,
        };
        assert_eq!(
            has_valid_transfer_args(&TransactionArgs::Named(args), min_motes),
            Err(expected_error)
        );

        // Wrong "source" type (an optional arg).
        let args = runtime_args! {
            TRANSFER_ARG_AMOUNT.name => U512::from(min_motes),
            TRANSFER_ARG_SOURCE.name => 1_u8,
            TRANSFER_ARG_TARGET => PublicKey::random(rng)
        };
        let expected_error = InvalidTransactionV1::UnexpectedArgType {
            arg_name: TRANSFER_ARG_SOURCE.name.to_string(),
            expected: vec![URef::cl_type()],
            got: CLType::U8,
        };
        assert_eq!(
            has_valid_transfer_args(&TransactionArgs::Named(args), min_motes),
            Err(expected_error)
        );
    }

    #[test]
    fn should_validate_add_bid_args() {
        let rng = &mut TestRng::new();

        let minimum_delegation_amount = rng.gen::<bool>().then(|| rng.gen());
        let maximum_delegation_amount = minimum_delegation_amount
            .map(|minimum_delegation_amount| minimum_delegation_amount + rng.gen::<u32>() as u64);
        let reserved_slots = rng.gen::<bool>().then(|| rng.gen::<u32>());

        // Check random args.
        let mut args = new_add_bid_args(
            PublicKey::random(rng),
            rng.gen(),
            rng.gen::<u64>(),
            minimum_delegation_amount,
            maximum_delegation_amount,
            reserved_slots,
        )
        .unwrap();
        has_valid_add_bid_args(&TransactionArgs::Named(args.clone())).unwrap();

        // Check with extra arg.
        args.insert("a", 1).unwrap();
        has_valid_add_bid_args(&TransactionArgs::Named(args)).unwrap();
    }

    #[test]
    fn add_bid_args_with_missing_required_should_be_invalid() {
        let rng = &mut TestRng::new();

        // Missing "public_key".
        let args = runtime_args! {
            ADD_BID_ARG_DELEGATION_RATE.name => rng.gen::<u8>(),
            ADD_BID_ARG_AMOUNT.name => U512::from(rng.gen::<u64>())
        };
        let expected_error = InvalidTransactionV1::MissingArg {
            arg_name: ADD_BID_ARG_PUBLIC_KEY.name.to_string(),
        };
        assert_eq!(
            has_valid_add_bid_args(&TransactionArgs::Named(args)),
            Err(expected_error)
        );

        // Missing "delegation_rate".
        let args = runtime_args! {
            ADD_BID_ARG_PUBLIC_KEY.name => PublicKey::random(rng),
            ADD_BID_ARG_AMOUNT.name => U512::from(rng.gen::<u64>())
        };
        let expected_error = InvalidTransactionV1::MissingArg {
            arg_name: ADD_BID_ARG_DELEGATION_RATE.name.to_string(),
        };
        assert_eq!(
            has_valid_add_bid_args(&TransactionArgs::Named(args)),
            Err(expected_error)
        );

        // Missing "amount".
        let args = runtime_args! {
            ADD_BID_ARG_PUBLIC_KEY.name => PublicKey::random(rng),
            ADD_BID_ARG_DELEGATION_RATE.name => rng.gen::<u8>()
        };
        let expected_error = InvalidTransactionV1::MissingArg {
            arg_name: ADD_BID_ARG_AMOUNT.name.to_string(),
        };
        assert_eq!(
            has_valid_add_bid_args(&TransactionArgs::Named(args)),
            Err(expected_error)
        );
    }

    #[test]
    fn add_bid_args_with_wrong_type_should_be_invalid() {
        let rng = &mut TestRng::new();

        // Wrong "amount" type.
        let args = runtime_args! {
            ADD_BID_ARG_PUBLIC_KEY.name => PublicKey::random(rng),
            ADD_BID_ARG_DELEGATION_RATE.name => rng.gen::<u8>(),
            ADD_BID_ARG_AMOUNT.name => rng.gen::<u64>()
        };
        let expected_error = InvalidTransactionV1::UnexpectedArgType {
            arg_name: ADD_BID_ARG_AMOUNT.name.to_string(),
            expected: vec![CLType::U512],
            got: CLType::U64,
        };
        assert_eq!(
            has_valid_add_bid_args(&TransactionArgs::Named(args)),
            Err(expected_error)
        );
    }

    #[test]
    fn should_validate_withdraw_bid_args() {
        let rng = &mut TestRng::new();

        // Check random args.
        let mut args = new_withdraw_bid_args(PublicKey::random(rng), rng.gen::<u64>()).unwrap();
        has_valid_withdraw_bid_args(&TransactionArgs::Named(args.clone())).unwrap();

        // Check with extra arg.
        args.insert("a", 1).unwrap();
        has_valid_withdraw_bid_args(&TransactionArgs::Named(args)).unwrap();
    }

    #[test]
    fn withdraw_bid_args_with_missing_required_should_be_invalid() {
        let rng = &mut TestRng::new();

        // Missing "public_key".
        let args = runtime_args! {
            WITHDRAW_BID_ARG_AMOUNT.name => U512::from(rng.gen::<u64>())
        };
        let expected_error = InvalidTransactionV1::MissingArg {
            arg_name: WITHDRAW_BID_ARG_PUBLIC_KEY.name.to_string(),
        };
        assert_eq!(
            has_valid_withdraw_bid_args(&TransactionArgs::Named(args)),
            Err(expected_error)
        );

        // Missing "amount".
        let args = runtime_args! {
            WITHDRAW_BID_ARG_PUBLIC_KEY.name => PublicKey::random(rng),
        };
        let expected_error = InvalidTransactionV1::MissingArg {
            arg_name: WITHDRAW_BID_ARG_AMOUNT.name.to_string(),
        };
        assert_eq!(
            has_valid_withdraw_bid_args(&TransactionArgs::Named(args)),
            Err(expected_error)
        );
    }

    #[test]
    fn withdraw_bid_args_with_wrong_type_should_be_invalid() {
        let rng = &mut TestRng::new();

        // Wrong "amount" type.
        let args = runtime_args! {
            WITHDRAW_BID_ARG_PUBLIC_KEY.name => PublicKey::random(rng),
            WITHDRAW_BID_ARG_AMOUNT.name => rng.gen::<u64>()
        };
        let expected_error = InvalidTransactionV1::UnexpectedArgType {
            arg_name: WITHDRAW_BID_ARG_AMOUNT.name.to_string(),
            expected: vec![CLType::U512],
            got: CLType::U64,
        };
        assert_eq!(
            has_valid_withdraw_bid_args(&TransactionArgs::Named(args)),
            Err(expected_error)
        );
    }

    #[test]
    fn should_validate_delegate_args() {
        let rng = &mut TestRng::new();

        // Check random args.
        let mut args = new_delegate_args(
            PublicKey::random(rng),
            PublicKey::random(rng),
            rng.gen::<u64>(),
        )
        .unwrap();
        has_valid_delegate_args(&TransactionArgs::Named(args.clone())).unwrap();

        // Check with extra arg.
        args.insert("a", 1).unwrap();
        has_valid_delegate_args(&TransactionArgs::Named(args)).unwrap();
    }

    #[test]
    fn delegate_args_with_missing_required_should_be_invalid() {
        let rng = &mut TestRng::new();

        // Missing "delegator".
        let args = runtime_args! {
            DELEGATE_ARG_VALIDATOR.name => PublicKey::random(rng),
            DELEGATE_ARG_AMOUNT.name => U512::from(rng.gen::<u64>())
        };
        let expected_error = InvalidTransactionV1::MissingArg {
            arg_name: DELEGATE_ARG_DELEGATOR.name.to_string(),
        };
        assert_eq!(
            has_valid_delegate_args(&TransactionArgs::Named(args)),
            Err(expected_error)
        );

        // Missing "validator".
        let args = runtime_args! {
            DELEGATE_ARG_DELEGATOR.name => PublicKey::random(rng),
            DELEGATE_ARG_AMOUNT.name => U512::from(rng.gen::<u64>())
        };
        let expected_error = InvalidTransactionV1::MissingArg {
            arg_name: DELEGATE_ARG_VALIDATOR.name.to_string(),
        };
        assert_eq!(
            has_valid_delegate_args(&TransactionArgs::Named(args)),
            Err(expected_error)
        );

        // Missing "amount".
        let args = runtime_args! {
            DELEGATE_ARG_DELEGATOR.name => PublicKey::random(rng),
            DELEGATE_ARG_VALIDATOR.name => PublicKey::random(rng),
        };
        let expected_error = InvalidTransactionV1::MissingArg {
            arg_name: DELEGATE_ARG_AMOUNT.name.to_string(),
        };
        assert_eq!(
            has_valid_delegate_args(&TransactionArgs::Named(args)),
            Err(expected_error)
        );
    }

    #[test]
    fn delegate_args_with_wrong_type_should_be_invalid() {
        let rng = &mut TestRng::new();

        // Wrong "amount" type.
        let args = runtime_args! {
            DELEGATE_ARG_DELEGATOR.name => PublicKey::random(rng),
            DELEGATE_ARG_VALIDATOR.name => PublicKey::random(rng),
            DELEGATE_ARG_AMOUNT.name => rng.gen::<u64>()
        };
        let expected_error = InvalidTransactionV1::UnexpectedArgType {
            arg_name: DELEGATE_ARG_AMOUNT.name.to_string(),
            expected: vec![CLType::U512],
            got: CLType::U64,
        };
        assert_eq!(
            has_valid_delegate_args(&TransactionArgs::Named(args)),
            Err(expected_error)
        );
    }

    #[test]
    fn should_validate_undelegate_args() {
        let rng = &mut TestRng::new();

        // Check random args.
        let mut args = new_undelegate_args(
            PublicKey::random(rng),
            PublicKey::random(rng),
            rng.gen::<u64>(),
        )
        .unwrap();
        has_valid_undelegate_args(&TransactionArgs::Named(args.clone())).unwrap();

        // Check with extra arg.
        args.insert("a", 1).unwrap();
        has_valid_undelegate_args(&TransactionArgs::Named(args)).unwrap();
    }

    #[test]
    fn undelegate_args_with_missing_required_should_be_invalid() {
        let rng = &mut TestRng::new();

        // Missing "delegator".
        let args = runtime_args! {
            UNDELEGATE_ARG_VALIDATOR.name => PublicKey::random(rng),
            UNDELEGATE_ARG_AMOUNT.name => U512::from(rng.gen::<u64>())
        };
        let expected_error = InvalidTransactionV1::MissingArg {
            arg_name: UNDELEGATE_ARG_DELEGATOR.name.to_string(),
        };
        assert_eq!(
            has_valid_undelegate_args(&TransactionArgs::Named(args)),
            Err(expected_error)
        );

        // Missing "validator".
        let args = runtime_args! {
            UNDELEGATE_ARG_DELEGATOR.name => PublicKey::random(rng),
            UNDELEGATE_ARG_AMOUNT.name => U512::from(rng.gen::<u64>())
        };
        let expected_error = InvalidTransactionV1::MissingArg {
            arg_name: UNDELEGATE_ARG_VALIDATOR.name.to_string(),
        };
        assert_eq!(
            has_valid_undelegate_args(&TransactionArgs::Named(args)),
            Err(expected_error)
        );

        // Missing "amount".
        let args = runtime_args! {
            UNDELEGATE_ARG_DELEGATOR.name => PublicKey::random(rng),
            UNDELEGATE_ARG_VALIDATOR.name => PublicKey::random(rng),
        };
        let expected_error = InvalidTransactionV1::MissingArg {
            arg_name: UNDELEGATE_ARG_AMOUNT.name.to_string(),
        };
        assert_eq!(
            has_valid_undelegate_args(&TransactionArgs::Named(args)),
            Err(expected_error)
        );
    }

    #[test]
    fn undelegate_args_with_wrong_type_should_be_invalid() {
        let rng = &mut TestRng::new();

        // Wrong "amount" type.
        let args = runtime_args! {
            UNDELEGATE_ARG_DELEGATOR.name => PublicKey::random(rng),
            UNDELEGATE_ARG_VALIDATOR.name => PublicKey::random(rng),
            UNDELEGATE_ARG_AMOUNT.name => rng.gen::<u64>()
        };
        let expected_error = InvalidTransactionV1::UnexpectedArgType {
            arg_name: UNDELEGATE_ARG_AMOUNT.name.to_string(),
            expected: vec![CLType::U512],
            got: CLType::U64,
        };
        assert_eq!(
            has_valid_undelegate_args(&TransactionArgs::Named(args)),
            Err(expected_error)
        );
    }

    #[test]
    fn should_validate_redelegate_args() {
        let rng = &mut TestRng::new();

        // Check random args.
        let mut args = new_redelegate_args(
            PublicKey::random(rng),
            PublicKey::random(rng),
            rng.gen::<u64>(),
            PublicKey::random(rng),
        )
        .unwrap();
        has_valid_redelegate_args(&TransactionArgs::Named(args.clone())).unwrap();

        // Check with extra arg.
        args.insert("a", 1).unwrap();
        has_valid_redelegate_args(&TransactionArgs::Named(args)).unwrap();
    }

    #[test]
    fn redelegate_args_with_missing_required_should_be_invalid() {
        let rng = &mut TestRng::new();

        // Missing "delegator".
        let args = runtime_args! {
            REDELEGATE_ARG_VALIDATOR.name => PublicKey::random(rng),
            REDELEGATE_ARG_AMOUNT.name => U512::from(rng.gen::<u64>()),
            REDELEGATE_ARG_NEW_VALIDATOR.name => PublicKey::random(rng),
        };
        let expected_error = InvalidTransactionV1::MissingArg {
            arg_name: REDELEGATE_ARG_DELEGATOR.name.to_string(),
        };
        assert_eq!(
            has_valid_redelegate_args(&TransactionArgs::Named(args)),
            Err(expected_error)
        );

        // Missing "validator".
        let args = runtime_args! {
            REDELEGATE_ARG_DELEGATOR.name => PublicKey::random(rng),
            REDELEGATE_ARG_AMOUNT.name => U512::from(rng.gen::<u64>()),
            REDELEGATE_ARG_NEW_VALIDATOR.name => PublicKey::random(rng),
        };
        let expected_error = InvalidTransactionV1::MissingArg {
            arg_name: REDELEGATE_ARG_VALIDATOR.name.to_string(),
        };
        assert_eq!(
            has_valid_redelegate_args(&TransactionArgs::Named(args)),
            Err(expected_error)
        );

        // Missing "amount".
        let args = runtime_args! {
            REDELEGATE_ARG_DELEGATOR.name => PublicKey::random(rng),
            REDELEGATE_ARG_VALIDATOR.name => PublicKey::random(rng),
            REDELEGATE_ARG_NEW_VALIDATOR.name => PublicKey::random(rng),
        };
        let expected_error = InvalidTransactionV1::MissingArg {
            arg_name: REDELEGATE_ARG_AMOUNT.name.to_string(),
        };
        assert_eq!(
            has_valid_redelegate_args(&TransactionArgs::Named(args)),
            Err(expected_error)
        );

        // Missing "new_validator".
        let args = runtime_args! {
            REDELEGATE_ARG_DELEGATOR.name => PublicKey::random(rng),
            REDELEGATE_ARG_VALIDATOR.name => PublicKey::random(rng),
            REDELEGATE_ARG_AMOUNT.name => U512::from(rng.gen::<u64>()),
        };
        let expected_error = InvalidTransactionV1::MissingArg {
            arg_name: REDELEGATE_ARG_NEW_VALIDATOR.name.to_string(),
        };
        assert_eq!(
            has_valid_redelegate_args(&TransactionArgs::Named(args)),
            Err(expected_error)
        );
    }

    #[test]
    fn redelegate_args_with_wrong_type_should_be_invalid() {
        let rng = &mut TestRng::new();

        // Wrong "amount" type.
        let args = runtime_args! {
            REDELEGATE_ARG_DELEGATOR.name => PublicKey::random(rng),
            REDELEGATE_ARG_VALIDATOR.name => PublicKey::random(rng),
            REDELEGATE_ARG_AMOUNT.name => rng.gen::<u64>(),
            REDELEGATE_ARG_NEW_VALIDATOR.name => PublicKey::random(rng),
        };
        let expected_error = InvalidTransactionV1::UnexpectedArgType {
            arg_name: REDELEGATE_ARG_AMOUNT.name.to_string(),
            expected: vec![CLType::U512],
            got: CLType::U64,
        };
        assert_eq!(
            has_valid_redelegate_args(&TransactionArgs::Named(args)),
            Err(expected_error)
        );
    }

    #[test]
    fn should_validate_change_bid_public_key_args() {
        let rng = &mut TestRng::new();

        // Check random args.
        let mut args =
            new_change_bid_public_key_args(PublicKey::random(rng), PublicKey::random(rng)).unwrap();
        has_valid_change_bid_public_key_args(&TransactionArgs::Named(args.clone())).unwrap();

        // Check with extra arg.
        args.insert("a", 1).unwrap();
        has_valid_change_bid_public_key_args(&TransactionArgs::Named(args)).unwrap();
    }

    #[test]
    fn change_bid_public_key_args_with_missing_required_should_be_invalid() {
        let rng = &mut TestRng::new();

        // Missing "public_key".
        let args = runtime_args! {
            CHANGE_BID_PUBLIC_KEY_ARG_NEW_PUBLIC_KEY.name => PublicKey::random(rng),
        };
        let expected_error = InvalidTransactionV1::MissingArg {
            arg_name: CHANGE_BID_PUBLIC_KEY_ARG_PUBLIC_KEY.name.to_string(),
        };
        assert_eq!(
            has_valid_change_bid_public_key_args(&TransactionArgs::Named(args)),
            Err(expected_error)
        );

        // Missing "new_public_key".
        let args = runtime_args! {
            CHANGE_BID_PUBLIC_KEY_ARG_PUBLIC_KEY.name => PublicKey::random(rng),
        };
        let expected_error = InvalidTransactionV1::MissingArg {
            arg_name: CHANGE_BID_PUBLIC_KEY_ARG_NEW_PUBLIC_KEY.name.to_string(),
        };
        assert_eq!(
            has_valid_change_bid_public_key_args(&TransactionArgs::Named(args)),
            Err(expected_error)
        );
    }

    #[test]
    fn change_bid_public_key_args_with_wrong_type_should_be_invalid() {
        let rng = &mut TestRng::new();

        // Wrong "public_key" type.
        let args = runtime_args! {
            CHANGE_BID_PUBLIC_KEY_ARG_PUBLIC_KEY.name => rng.gen::<u8>(),
            CHANGE_BID_PUBLIC_KEY_ARG_NEW_PUBLIC_KEY.name => PublicKey::random(rng),
        };
        let expected_error = InvalidTransactionV1::UnexpectedArgType {
            arg_name: CHANGE_BID_PUBLIC_KEY_ARG_PUBLIC_KEY.name.to_string(),
            expected: vec![CLType::PublicKey],
            got: CLType::U8,
        };
        assert_eq!(
            has_valid_change_bid_public_key_args(&TransactionArgs::Named(args)),
            Err(expected_error)
        );

        // Wrong "new_public_key" type.
        let args = runtime_args! {
            CHANGE_BID_PUBLIC_KEY_ARG_PUBLIC_KEY.name => PublicKey::random(rng),
            CHANGE_BID_PUBLIC_KEY_ARG_NEW_PUBLIC_KEY.name => rng.gen::<u8>(),
        };
        let expected_error = InvalidTransactionV1::UnexpectedArgType {
            arg_name: CHANGE_BID_PUBLIC_KEY_ARG_NEW_PUBLIC_KEY.name.to_string(),
            expected: vec![CLType::PublicKey],
            got: CLType::U8,
        };
        assert_eq!(
            has_valid_change_bid_public_key_args(&TransactionArgs::Named(args)),
            Err(expected_error)
        );
    }

    #[test]
    fn should_validate_add_reservations_args() {
        let rng = &mut TestRng::new();

        let reservations = rng.random_vec(1..100);

        // Check random args.
        let mut args = new_add_reservations_args(reservations).unwrap();
        has_valid_add_reservations_args(&TransactionArgs::Named(args.clone())).unwrap();

        // Check with extra arg.
        args.insert("a", 1).unwrap();
        has_valid_add_reservations_args(&TransactionArgs::Named(args)).unwrap();
    }

    #[test]
    fn add_reservations_args_with_missing_required_should_be_invalid() {
        // Missing "reservations".
        let args = runtime_args! {};
        let expected_error = InvalidTransactionV1::MissingArg {
            arg_name: ADD_RESERVATIONS_ARG_RESERVATIONS.name.to_string(),
        };
        assert_eq!(
            has_valid_add_reservations_args(&TransactionArgs::Named(args)),
            Err(expected_error)
        );
    }

    #[test]
    fn add_reservations_args_with_wrong_type_should_be_invalid() {
        let rng = &mut TestRng::new();

        // Wrong "reservations" type.
        let args = runtime_args! {
            ADD_RESERVATIONS_ARG_RESERVATIONS.name => PublicKey::random(rng),
        };
        let expected_error = InvalidTransactionV1::UnexpectedArgType {
            arg_name: ADD_RESERVATIONS_ARG_RESERVATIONS.name.to_string(),
            expected: vec![CLType::List(Box::new(CLType::Any))],
            got: CLType::PublicKey,
        };
        assert_eq!(
            has_valid_add_reservations_args(&TransactionArgs::Named(args)),
            Err(expected_error)
        );
    }

    #[test]
    fn should_validate_cancel_reservations_args() {
        let rng = &mut TestRng::new();

        let validator = PublicKey::random(rng);
        let delegators = rng.random_vec(0..100);

        // Check random args.
        let mut args = new_cancel_reservations_args(validator, delegators).unwrap();
        has_valid_cancel_reservations_args(&TransactionArgs::Named(args.clone())).unwrap();

        // Check with extra arg.
        args.insert("a", 1).unwrap();
        has_valid_cancel_reservations_args(&TransactionArgs::Named(args)).unwrap();
    }

    #[test]
    fn cancel_reservations_args_with_missing_required_should_be_invalid() {
        let rng = &mut TestRng::new();

        // Missing "validator".
        let args = runtime_args! {
            CANCEL_RESERVATIONS_ARG_DELEGATORS.name  => rng.random_vec::<Range<usize>, DelegatorKind>(0..100),
        };
        let expected_error = InvalidTransactionV1::MissingArg {
            arg_name: CANCEL_RESERVATIONS_ARG_VALIDATOR.name.to_string(),
        };
        assert_eq!(
            has_valid_cancel_reservations_args(&TransactionArgs::Named(args)),
            Err(expected_error)
        );

        // Missing "delegators".
        let args = runtime_args! {
            CANCEL_RESERVATIONS_ARG_VALIDATOR.name => PublicKey::random(rng),
        };
        let expected_error = InvalidTransactionV1::MissingArg {
            arg_name: CANCEL_RESERVATIONS_ARG_DELEGATORS.name.to_string(),
        };
        assert_eq!(
            has_valid_cancel_reservations_args(&TransactionArgs::Named(args)),
            Err(expected_error)
        );
    }

    #[test]
    fn cancel_reservations_args_with_wrong_type_should_be_invalid() {
        let rng = &mut TestRng::new();

        // Wrong "validator" type.
        let args = runtime_args! {
            CANCEL_RESERVATIONS_ARG_VALIDATOR.name => rng.random_vec::<Range<usize>, PublicKey>(0..100),
            CANCEL_RESERVATIONS_ARG_DELEGATORS.name => rng.random_vec::<Range<usize>, DelegatorKind>(0..100),
        };
        let expected_error = InvalidTransactionV1::UnexpectedArgType {
            arg_name: CANCEL_RESERVATIONS_ARG_VALIDATOR.name.to_string(),
            expected: vec![CLType::PublicKey],
            got: CLType::List(Box::new(CLType::PublicKey)),
        };
        assert_eq!(
            has_valid_cancel_reservations_args(&TransactionArgs::Named(args)),
            Err(expected_error)
        );

        // Wrong "delegators" type.
        let args = runtime_args! {
            CANCEL_RESERVATIONS_ARG_VALIDATOR.name => PublicKey::random(rng),
            CANCEL_RESERVATIONS_ARG_DELEGATORS.name => rng.gen::<u8>(),
        };
        let expected_error = InvalidTransactionV1::UnexpectedArgType {
            arg_name: CANCEL_RESERVATIONS_ARG_DELEGATORS.name.to_string(),
            expected: vec![CLType::List(Box::new(CLType::Any))],
            got: CLType::U8,
        };
        assert_eq!(
            has_valid_cancel_reservations_args(&TransactionArgs::Named(args)),
            Err(expected_error)
        );
    }

    #[test]
    fn native_calls_require_named_args() {
        let args = TransactionArgs::Bytesrepr(vec![b'a'; 100].into());
        let expected_error = InvalidTransactionV1::ExpectedNamedArguments;
        assert_eq!(
            has_valid_transfer_args(&args, 0).as_ref(),
            Err(&expected_error)
        );
        assert_eq!(has_valid_add_bid_args(&args).as_ref(), Err(&expected_error));
        assert_eq!(
            has_valid_withdraw_bid_args(&args).as_ref(),
            Err(&expected_error)
        );
        assert_eq!(
            has_valid_delegate_args(&args).as_ref(),
            Err(&expected_error)
        );
        assert_eq!(
            has_valid_undelegate_args(&args).as_ref(),
            Err(&expected_error)
        );
        assert_eq!(
            has_valid_redelegate_args(&args).as_ref(),
            Err(&expected_error)
        );
        assert_eq!(
            has_valid_add_reservations_args(&args).as_ref(),
            Err(&expected_error)
        );
        assert_eq!(
            has_valid_cancel_reservations_args(&args).as_ref(),
            Err(&expected_error)
        );
    }
}
