use casper_types::{
    InvalidTransactionV1, PricingMode, TransactionEntryPoint, TransactionRuntimeParams,
    TransactionTarget, TransactionV1Config, AUCTION_LANE_ID, INSTALL_UPGRADE_LANE_ID, MINT_LANE_ID,
};

/// Calculates the laned based on properties of the transaction
pub(crate) fn calculate_transaction_lane(
    entry_point: &TransactionEntryPoint,
    target: &TransactionTarget,
    pricing_mode: &PricingMode,
    config: &TransactionV1Config,
    size_estimation: u64,
) -> Result<u8, InvalidTransactionV1> {
    match target {
        TransactionTarget::Native => match entry_point {
            TransactionEntryPoint::Transfer => Ok(MINT_LANE_ID),
            TransactionEntryPoint::AddBid
            | TransactionEntryPoint::WithdrawBid
            | TransactionEntryPoint::Delegate
            | TransactionEntryPoint::Undelegate
            | TransactionEntryPoint::Redelegate
            | TransactionEntryPoint::ActivateBid
            | TransactionEntryPoint::ChangeBidPublicKey
            | TransactionEntryPoint::AddReservations
            | TransactionEntryPoint::CancelReservations => Ok(AUCTION_LANE_ID),
            TransactionEntryPoint::Call => Err(InvalidTransactionV1::EntryPointCannotBeCall),
            TransactionEntryPoint::Custom(_) => {
                Err(InvalidTransactionV1::EntryPointCannotBeCustom {
                    entry_point: entry_point.clone(),
                })
            }
        },
        TransactionTarget::Stored { .. } => match entry_point {
            TransactionEntryPoint::Custom(_) => {
                get_lane_for_non_install_wasm(config, size_estimation, pricing_mode)
            }
            TransactionEntryPoint::Call
            | TransactionEntryPoint::Transfer
            | TransactionEntryPoint::AddBid
            | TransactionEntryPoint::WithdrawBid
            | TransactionEntryPoint::Delegate
            | TransactionEntryPoint::Undelegate
            | TransactionEntryPoint::Redelegate
            | TransactionEntryPoint::ActivateBid
            | TransactionEntryPoint::ChangeBidPublicKey
            | TransactionEntryPoint::AddReservations
            | TransactionEntryPoint::CancelReservations => {
                Err(InvalidTransactionV1::EntryPointMustBeCustom {
                    entry_point: entry_point.clone(),
                })
            }
        },
        TransactionTarget::Session {
            is_install_upgrade,
            runtime: TransactionRuntimeParams::VmCasperV1,
            ..
        } => match entry_point {
            TransactionEntryPoint::Call => {
                if *is_install_upgrade {
                    Ok(INSTALL_UPGRADE_LANE_ID)
                } else {
                    get_lane_for_non_install_wasm(config, size_estimation, pricing_mode)
                }
            }
            TransactionEntryPoint::Custom(_)
            | TransactionEntryPoint::Transfer
            | TransactionEntryPoint::AddBid
            | TransactionEntryPoint::WithdrawBid
            | TransactionEntryPoint::Delegate
            | TransactionEntryPoint::Undelegate
            | TransactionEntryPoint::Redelegate
            | TransactionEntryPoint::ActivateBid
            | TransactionEntryPoint::ChangeBidPublicKey
            | TransactionEntryPoint::AddReservations
            | TransactionEntryPoint::CancelReservations => {
                Err(InvalidTransactionV1::EntryPointMustBeCall {
                    entry_point: entry_point.clone(),
                })
            }
        },
        TransactionTarget::Session {
            is_install_upgrade,
            runtime: TransactionRuntimeParams::VmCasperV2 { .. },
            ..
        } => match entry_point {
            TransactionEntryPoint::Call | TransactionEntryPoint::Custom(_) => {
                if *is_install_upgrade {
                    Ok(INSTALL_UPGRADE_LANE_ID)
                } else {
                    get_lane_for_non_install_wasm(config, size_estimation, pricing_mode)
                }
            }
            TransactionEntryPoint::Transfer
            | TransactionEntryPoint::AddBid
            | TransactionEntryPoint::WithdrawBid
            | TransactionEntryPoint::Delegate
            | TransactionEntryPoint::Undelegate
            | TransactionEntryPoint::Redelegate
            | TransactionEntryPoint::ActivateBid
            | TransactionEntryPoint::ChangeBidPublicKey
            | TransactionEntryPoint::AddReservations
            | TransactionEntryPoint::CancelReservations => {
                Err(InvalidTransactionV1::EntryPointMustBeCall {
                    entry_point: entry_point.clone(),
                })
            }
        },
    }
}

fn get_lane_for_non_install_wasm(
    config: &TransactionV1Config,
    transaction_size: u64,
    pricing_mode: &PricingMode,
) -> Result<u8, InvalidTransactionV1> {
    match pricing_mode {
        PricingMode::PaymentLimited { payment_amount, .. } => config
            .get_wasm_lane_id_by_payment_limited(*payment_amount, transaction_size)
            .ok_or(InvalidTransactionV1::NoWasmLaneMatchesTransaction()),
        PricingMode::Fixed {
            additional_computation_factor,
            ..
        } => config
            .get_wasm_lane_id_by_size(transaction_size, *additional_computation_factor)
            .ok_or(InvalidTransactionV1::NoWasmLaneMatchesTransaction()),
        PricingMode::Prepaid { .. } => Err(InvalidTransactionV1::PricingModeNotSupported),
    }
}
