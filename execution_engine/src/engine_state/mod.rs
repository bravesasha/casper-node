//!  This module contains all the execution related code.
pub mod deploy_item;
pub mod engine_config;
mod error;
pub mod execute_request;
pub(crate) mod execution_kind;
pub mod execution_result;
pub mod genesis;
mod prune;
pub mod run_genesis_request;
pub mod step;
mod transfer;
pub mod upgrade;

use itertools::Itertools;

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    convert::TryFrom,
    rc::Rc,
};

use num_rational::Ratio;
use num_traits::Zero;
use once_cell::sync::Lazy;
use tracing::{debug, error, trace, warn};

use casper_storage::{
    data_access_layer::{
        balance::BalanceResult,
        get_bids::{BidsRequest, BidsResult},
        query::{QueryRequest, QueryResult},
        DataAccessLayer, EraValidatorsRequest, EraValidatorsResult,
    },
    global_state::{
        self,
        state::{
            lmdb::LmdbGlobalState, scratch::ScratchGlobalState, CommitProvider, StateProvider,
            StateReader,
        },
        trie::{merkle_proof::TrieMerkleProof, TrieRaw},
        trie_store::operations::PruneResult as GlobalStatePruneResult,
    },
    system::auction,
    tracking_copy::{TrackingCopy, TrackingCopyError, TrackingCopyExt},
    AddressGenerator,
};

use casper_types::{
    account::{Account, AccountHash},
    addressable_entity::{
        ActionThresholds, AssociatedKeys, EntityKind, EntityKindTag, MessageTopics, NamedKeyAddr,
        NamedKeyValue, NamedKeys, Weight,
    },
    bytesrepr::ToBytes,
    execution::Effects,
    package::{EntityVersions, Groups, PackageStatus},
    system::{
        auction::{
            BidAddr, BidKind, ValidatorBid, ARG_ERA_END_TIMESTAMP_MILLIS, ARG_EVICTED_VALIDATORS,
            ARG_REWARDS_MAP, ARG_VALIDATOR_PUBLIC_KEYS, AUCTION_DELAY_KEY, LOCKED_FUNDS_PERIOD_KEY,
            SEIGNIORAGE_RECIPIENTS_SNAPSHOT_KEY, UNBONDING_DELAY_KEY, VALIDATOR_SLOTS_KEY,
        },
        handle_payment::{self, ACCUMULATION_PURSE_KEY},
        mint::{self, ROUND_SEIGNIORAGE_RATE_KEY},
        AUCTION, HANDLE_PAYMENT, MINT,
    },
    AccessRights, AddressableEntity, AddressableEntityHash, ApiError, BlockTime, ByteCodeHash,
    CLValue, ChainspecRegistry, ChecksumRegistry, DeployHash, DeployInfo, Digest, EntityAddr,
    EntryPoints, ExecutableDeployItem, FeeHandling, Gas, Key, KeyTag, Motes, Package, PackageHash,
    Phase, ProtocolVersion, PublicKey, RuntimeArgs, StoredValue, SystemContractRegistry, URef,
    UpgradeConfig, U512,
};

use self::transfer::NewTransferTargetMode;
pub use self::{
    deploy_item::DeployItem,
    engine_config::{
        EngineConfig, EngineConfigBuilder, DEFAULT_MAX_QUERY_DEPTH,
        DEFAULT_MAX_RUNTIME_CALL_STACK_HEIGHT,
    },
    error::Error,
    execute_request::ExecuteRequest,
    execution::Error as ExecError,
    execution_result::{ExecutionResult, ForcedTransferResult},
    genesis::{ExecConfig, GenesisConfig, GenesisSuccess},
    prune::{PruneConfig, PruneResult},
    run_genesis_request::RunGenesisRequest,
    step::{RewardItem, SlashItem, StepError, StepRequest, StepSuccess},
    transfer::{TransferArgs, TransferRuntimeArgsBuilder, TransferTargetMode},
    upgrade::UpgradeSuccess,
};

use crate::{
    engine_state::{
        execution_kind::ExecutionKind,
        execution_result::{ExecutionResultBuilder, ExecutionResults},
        genesis::GenesisInstaller,
        upgrade::{ProtocolUpgradeError, SystemUpgrader},
    },
    execution::{self, DirectSystemContractCall, Executor},
    runtime::RuntimeStack,
};

const DEFAULT_ADDRESS: [u8; 32] = [0; 32];
/// The maximum amount of motes that payment code execution can cost.
pub const MAX_PAYMENT_AMOUNT: u64 = 2_500_000_000;
/// The maximum amount of gas a payment code can use.
///
/// This value also indicates the minimum balance of the main purse of an account when
/// executing payment code, as such amount is held as collateral to compensate for
/// code execution.
pub static MAX_PAYMENT: Lazy<U512> = Lazy::new(|| U512::from(MAX_PAYMENT_AMOUNT));

/// A special contract wasm hash for contracts representing Accounts.
pub static ACCOUNT_BYTE_CODE_HASH: Lazy<ByteCodeHash> =
    Lazy::new(|| ByteCodeHash::new(DEFAULT_ADDRESS));

/// Gas/motes conversion rate of wasmless transfer cost is always 1 regardless of what user wants to
/// pay.
pub const WASMLESS_TRANSFER_FIXED_GAS_PRICE: u64 = 1;

/// Main implementation of an execution engine state.
///
/// Takes an engine's configuration and a provider of a state (aka the global state) to operate on.
/// Methods implemented on this structure are the external API intended to be used by the users such
/// as the node, test framework, and others.
#[derive(Debug)]
pub struct EngineState<S> {
    config: EngineConfig,
    state: S,
}

impl EngineState<ScratchGlobalState> {
    /// Returns the inner state
    pub fn into_inner(self) -> ScratchGlobalState {
        self.state
    }
}

impl EngineState<DataAccessLayer<LmdbGlobalState>> {
    /// Gets underlyng LmdbGlobalState
    pub fn get_state(&self) -> &DataAccessLayer<LmdbGlobalState> {
        &self.state
    }

    /// Flushes the LMDB environment to disk when manual sync is enabled in the config.toml.
    pub fn flush_environment(&self) -> Result<(), global_state::error::Error> {
        if self.state.state().environment().is_manual_sync_enabled() {
            self.state.state().environment().sync()?
        }
        Ok(())
    }

    /// Provide a local cached-only version of engine-state.
    pub fn get_scratch_engine_state(&self) -> EngineState<ScratchGlobalState> {
        EngineState {
            config: self.config.clone(),
            state: self.state.state().create_scratch(),
        }
    }

    /// Writes state cached in an `EngineState<ScratchEngineState>` to LMDB.
    pub fn write_scratch_to_db(
        &self,
        state_root_hash: Digest,
        scratch_global_state: ScratchGlobalState,
    ) -> Result<Digest, Error> {
        let (stored_values, keys_to_prune) = scratch_global_state.into_inner();

        let post_state_hash = self
            .state
            .state()
            .put_stored_values(state_root_hash, stored_values)?;

        if keys_to_prune.is_empty() {
            return Ok(post_state_hash);
        }
        let prune_keys = keys_to_prune.iter().cloned().collect_vec();
        match self.state.state().prune_keys(post_state_hash, &prune_keys) {
            Ok(result) => match result {
                GlobalStatePruneResult::Pruned(post_state_hash) => Ok(post_state_hash),
                GlobalStatePruneResult::DoesNotExist => Err(Error::FailedToPrune(prune_keys)),
                GlobalStatePruneResult::RootNotFound => Err(Error::RootNotFound(post_state_hash)),
            },
            Err(err) => Err(err.into()),
        }
    }
}

impl EngineState<LmdbGlobalState> {
    /// Gets underlying LmdbGlobalState
    pub fn get_state(&self) -> &LmdbGlobalState {
        &self.state
    }

    /// Flushes the LMDB environment to disk when manual sync is enabled in the config.toml.
    pub fn flush_environment(&self) -> Result<(), global_state::error::Error> {
        if self.state.environment().is_manual_sync_enabled() {
            self.state.environment().sync()?
        }
        Ok(())
    }

    /// Provide a local cached-only version of engine-state.
    pub fn get_scratch_engine_state(&self) -> EngineState<ScratchGlobalState> {
        EngineState {
            config: self.config.clone(),
            state: self.state.create_scratch(),
        }
    }

    /// Writes state cached in an `EngineState<ScratchEngineState>` to LMDB.
    pub fn write_scratch_to_db(
        &self,
        state_root_hash: Digest,
        scratch_global_state: ScratchGlobalState,
    ) -> Result<Digest, Error> {
        let (stored_values, keys_to_prune) = scratch_global_state.into_inner();
        let post_state_hash = match self.state.put_stored_values(state_root_hash, stored_values) {
            Ok(root_hash) => root_hash,
            Err(err) => {
                return Err(err.into());
            }
        };
        if keys_to_prune.is_empty() {
            return Ok(post_state_hash);
        }
        let prune_keys = keys_to_prune.iter().cloned().collect_vec();
        match self.state.prune_keys(post_state_hash, &prune_keys) {
            Ok(result) => match result {
                GlobalStatePruneResult::Pruned(post_state_hash) => Ok(post_state_hash),
                GlobalStatePruneResult::DoesNotExist => Err(Error::FailedToPrune(prune_keys)),
                GlobalStatePruneResult::RootNotFound => Err(Error::RootNotFound(post_state_hash)),
            },
            Err(err) => Err(err.into()),
        }
    }
}

impl<S> EngineState<S>
where
    S: StateProvider + CommitProvider,
{
    /// Creates new engine state.
    pub fn new(state: S, config: EngineConfig) -> EngineState<S> {
        EngineState { config, state }
    }

    /// Returns engine config.
    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    /// Updates current engine config with a new instance.
    pub fn update_config(&mut self, new_config: EngineConfig) {
        self.config = new_config
    }

    /// Commits genesis process.
    ///
    /// This process is run only once per network to initiate the system. By definition users are
    /// unable to execute smart contracts on a network without a genesis.
    ///
    /// Takes genesis configuration passed through [`ExecConfig`] and creates the system contracts,
    /// sets up the genesis accounts, and sets up the auction state based on that. At the end of
    /// the process, [`SystemContractRegistry`] is persisted under the special global state space
    /// [`Key::SystemContractRegistry`].
    ///
    /// Returns a [`GenesisSuccess`] for a successful operation, or an error otherwise.
    pub fn commit_genesis(
        &self,
        genesis_config_hash: Digest,
        protocol_version: ProtocolVersion,
        ee_config: &ExecConfig,
        chainspec_registry: ChainspecRegistry,
    ) -> Result<GenesisSuccess, Error> {
        // Preliminaries
        let initial_root_hash = self.state.empty_root();

        let tracking_copy = match self.tracking_copy(initial_root_hash) {
            Ok(Some(tracking_copy)) => Rc::new(RefCell::new(tracking_copy)),
            // NOTE: As genesis is run once per instance condition below is considered programming
            // error
            Ok(None) => panic!("state has not been initialized properly"),
            Err(error) => return Err(Error::TrackingCopy(error)),
        };

        let mut genesis_installer: GenesisInstaller<S> = GenesisInstaller::new(
            genesis_config_hash,
            protocol_version,
            ee_config.clone(),
            tracking_copy,
        );

        genesis_installer.install(chainspec_registry)?;

        // Commit the transforms.
        let effects = genesis_installer.finalize();

        let post_state_hash = self
            .state
            .commit(initial_root_hash, effects.clone())
            .map_err(Into::<execution::Error>::into)?;

        // Return the result
        Ok(GenesisSuccess {
            post_state_hash,
            effects,
        })
    }

    /// Commits upgrade.
    ///
    /// This process applies changes to the global state.
    ///
    /// Returns [`UpgradeSuccess`].
    pub fn commit_upgrade(&self, upgrade_config: UpgradeConfig) -> Result<UpgradeSuccess, Error> {
        // per specification:
        // https://casperlabs.atlassian.net/wiki/spaces/EN/pages/139854367/Upgrading+System+Contracts+Specification

        // 3.1.1.1.1.1 validate pre state hash exists
        // 3.1.2.1 get a tracking_copy at the provided pre_state_hash
        let pre_state_hash = upgrade_config.pre_state_hash();
        let tracking_copy = match self.tracking_copy(pre_state_hash)? {
            Some(tracking_copy) => Rc::new(RefCell::new(tracking_copy)),
            None => return Err(Error::RootNotFound(pre_state_hash)),
        };

        // 3.1.1.1.1.2 current protocol version is required
        let current_protocol_version = upgrade_config.current_protocol_version();

        // 3.1.1.1.1.3 activation point is not currently used by EE; skipping
        // 3.1.1.1.1.4 upgrade point protocol version validation
        let new_protocol_version = upgrade_config.new_protocol_version();

        let upgrade_check_result =
            current_protocol_version.check_next_version(&new_protocol_version);

        if upgrade_check_result.is_invalid() {
            return Err(Error::InvalidProtocolVersion(new_protocol_version));
        }

        let mut registry = if let Ok(registry) = tracking_copy.borrow_mut().get_system_contracts() {
            registry
        } else {
            // Check the upgrade config for the registry
            let upgrade_registry = upgrade_config
                .global_state_update()
                .get(&Key::SystemContractRegistry)
                .ok_or_else(|| {
                    error!("Registry is absent in upgrade config");
                    Error::ProtocolUpgrade(ProtocolUpgradeError::FailedToCreateSystemRegistry)
                })?
                .to_owned();
            if let StoredValue::CLValue(cl_registry) = upgrade_registry {
                CLValue::into_t::<SystemContractRegistry>(cl_registry).map_err(|error| {
                    let error_msg = format!("Conversion to system registry failed: {:?}", error);
                    error!("{}", error_msg);
                    Error::Bytesrepr(error_msg)
                })?
            } else {
                error!("Failed to create registry as StoreValue in upgrade config is not CLValue");
                return Err(Error::ProtocolUpgrade(
                    ProtocolUpgradeError::FailedToCreateSystemRegistry,
                ));
            }
        };

        let mint_hash = *registry.get(MINT).ok_or_else(|| {
            error!("Missing system mint contract hash");
            Error::MissingSystemContractHash(MINT.to_string())
        })?;
        let auction_hash = *registry.get(AUCTION).ok_or_else(|| {
            error!("Missing system auction contract hash");
            Error::MissingSystemContractHash(AUCTION.to_string())
        })?;

        let handle_payment_hash = *registry.get(HANDLE_PAYMENT).ok_or_else(|| {
            error!("Missing system handle payment contract hash");
            Error::MissingSystemContractHash(HANDLE_PAYMENT.to_string())
        })?;

        if let Some(standard_payment_hash) = registry.remove_standard_payment() {
            // Write the chainspec registry to global state
            let cl_value_chainspec_registry =
                CLValue::from_t(registry).map_err(|error| Error::Bytesrepr(error.to_string()))?;

            tracking_copy.borrow_mut().write(
                Key::SystemContractRegistry,
                StoredValue::CLValue(cl_value_chainspec_registry),
            );

            // Prune away standard payment from global state.
            tracking_copy
                .borrow_mut()
                .prune(Key::Hash(standard_payment_hash.value()));
        };

        // Write the chainspec registry to global state
        let cl_value_chainspec_registry =
            CLValue::from_t(upgrade_config.chainspec_registry().clone())
                .map_err(|error| Error::Bytesrepr(error.to_string()))?;

        tracking_copy.borrow_mut().write(
            Key::ChainspecRegistry,
            StoredValue::CLValue(cl_value_chainspec_registry),
        );

        // Cycle through the system contracts and update
        // their metadata if there is a change in entry points.
        let system_upgrader: SystemUpgrader<S> = SystemUpgrader::new(
            new_protocol_version,
            current_protocol_version,
            tracking_copy.clone(),
        );

        system_upgrader.migrate_system_account(pre_state_hash)?;

        system_upgrader
            .create_accumulation_purse_if_required(&handle_payment_hash, &self.config)
            .map_err(Error::ProtocolUpgrade)?;

        system_upgrader
            .refresh_system_contracts(&mint_hash, &auction_hash, &handle_payment_hash)
            .map_err(Error::ProtocolUpgrade)?;

        // Prune away the standard payment record.

        // 3.1.1.1.1.7 new total validator slots is optional
        if let Some(new_validator_slots) = upgrade_config.new_validator_slots() {
            // 3.1.2.4 if new total validator slots is provided, update auction contract state
            let auction_addr = EntityAddr::new_system_entity_addr(auction_hash.value());

            let auction_named_keys = tracking_copy.borrow_mut().get_named_keys(auction_addr)?;

            let validator_slots_key = auction_named_keys
                .get(VALIDATOR_SLOTS_KEY)
                .expect("validator_slots key must exist in auction contract's named keys");
            let value = StoredValue::CLValue(
                CLValue::from_t(new_validator_slots)
                    .map_err(|_| Error::Bytesrepr("new_validator_slots".to_string()))?,
            );
            tracking_copy
                .borrow_mut()
                .write(*validator_slots_key, value);
        }

        if let Some(new_auction_delay) = upgrade_config.new_auction_delay() {
            debug!(%new_auction_delay, "Auction delay changed as part of the upgrade");

            let auction_addr = EntityAddr::new_system_entity_addr(auction_hash.value());

            let auction_named_keys = tracking_copy.borrow_mut().get_named_keys(auction_addr)?;

            let auction_delay_key = auction_named_keys
                .get(AUCTION_DELAY_KEY)
                .expect("auction_delay key must exist in auction contract's named keys");
            let value = StoredValue::CLValue(
                CLValue::from_t(new_auction_delay)
                    .map_err(|_| Error::Bytesrepr("new_auction_delay".to_string()))?,
            );
            tracking_copy.borrow_mut().write(*auction_delay_key, value);
        }

        if let Some(new_locked_funds_period) = upgrade_config.new_locked_funds_period_millis() {
            let auction_addr = EntityAddr::new_system_entity_addr(auction_hash.value());

            let auction_named_keys = tracking_copy.borrow_mut().get_named_keys(auction_addr)?;

            let locked_funds_period_key = auction_named_keys
                .get(LOCKED_FUNDS_PERIOD_KEY)
                .expect("locked_funds_period key must exist in auction contract's named keys");
            let value = StoredValue::CLValue(
                CLValue::from_t(new_locked_funds_period)
                    .map_err(|_| Error::Bytesrepr("new_locked_funds_period".to_string()))?,
            );
            tracking_copy
                .borrow_mut()
                .write(*locked_funds_period_key, value);
        }

        if let Some(new_round_seigniorage_rate) = upgrade_config.new_round_seigniorage_rate() {
            let new_round_seigniorage_rate: Ratio<U512> = {
                let (numer, denom) = new_round_seigniorage_rate.into();
                Ratio::new(numer.into(), denom.into())
            };

            let mint_addr = EntityAddr::new_system_entity_addr(mint_hash.value());

            let mint_named_keys = tracking_copy.borrow_mut().get_named_keys(mint_addr)?;

            let locked_funds_period_key = mint_named_keys
                .get(ROUND_SEIGNIORAGE_RATE_KEY)
                .expect("round_seigniorage_rate key must exist in mint contract's named keys");
            let value = StoredValue::CLValue(
                CLValue::from_t(new_round_seigniorage_rate)
                    .map_err(|_| Error::Bytesrepr("new_round_seigniorage_rate".to_string()))?,
            );
            tracking_copy
                .borrow_mut()
                .write(*locked_funds_period_key, value);
        }

        // One time upgrade of existing bids
        {
            let mut borrow = tracking_copy.borrow_mut();
            if let Ok(existing_bid_keys) = borrow.get_keys(&KeyTag::Bid) {
                for key in existing_bid_keys {
                    if let Some(StoredValue::Bid(existing_bid)) =
                        borrow.get(&key).map_err(Into::<Error>::into)?
                    {
                        // prune away the original record, we don't need it anymore
                        borrow.prune(key);

                        if existing_bid.staked_amount().is_zero() {
                            // the previous logic enforces unbonding all delegators of
                            // a validator that reduced their personal stake to 0 (and we have
                            // various existent tests that prove this), thus there is no need
                            // to handle the complicated hypothetical case of one or more
                            // delegator stakes being > 0 if the validator stake is 0.
                            //
                            // tl;dr this is a "zombie" bid and we don't need to continue
                            // carrying it forward at tip.
                            continue;
                        }

                        let validator_public_key = existing_bid.validator_public_key();
                        let validator_bid_addr = BidAddr::from(validator_public_key.clone());
                        let validator_bid = ValidatorBid::from(*existing_bid.clone());
                        borrow.write(
                            validator_bid_addr.into(),
                            StoredValue::BidKind(BidKind::Validator(Box::new(validator_bid))),
                        );

                        let delegators = existing_bid.delegators().clone();
                        for (_, delegator) in delegators {
                            let delegator_bid_addr = BidAddr::new_from_public_keys(
                                validator_public_key,
                                Some(delegator.delegator_public_key()),
                            );
                            // the previous code was removing a delegator bid from the embedded
                            // collection within their validator's bid when the delegator fully
                            // unstaked, so technically we don't need to check for 0 balance here.
                            // However, since it is low effort to check, doing it just to be sure.
                            if !delegator.staked_amount().is_zero() {
                                borrow.write(
                                    delegator_bid_addr.into(),
                                    StoredValue::BidKind(BidKind::Delegator(Box::new(delegator))),
                                );
                            }
                        }
                    }
                }
            }
        }

        // apply accepted global state updates (if any)
        for (key, value) in upgrade_config.global_state_update() {
            tracking_copy.borrow_mut().write(*key, value.clone());
        }

        // We insert the new unbonding delay once the purses to be paid out have been transformed
        // based on the previous unbonding delay.
        if let Some(new_unbonding_delay) = upgrade_config.new_unbonding_delay() {
            let auction_addr = EntityAddr::new_system_entity_addr(auction_hash.value());

            let auction_named_keys = tracking_copy.borrow_mut().get_named_keys(auction_addr)?;

            let unbonding_delay_key = auction_named_keys
                .get(UNBONDING_DELAY_KEY)
                .expect("unbonding_delay key must exist in auction contract's named keys");
            let value = StoredValue::CLValue(
                CLValue::from_t(new_unbonding_delay)
                    .map_err(|_| Error::Bytesrepr("new_unbonding_delay".to_string()))?,
            );
            tracking_copy
                .borrow_mut()
                .write(*unbonding_delay_key, value);
        }

        // EraInfo migration
        if let Some(activation_point) = upgrade_config.activation_point() {
            // The highest stored era is the immediate predecessor of the activation point.
            let highest_era_info_id = activation_point.saturating_sub(1);
            let highest_era_info_key = Key::EraInfo(highest_era_info_id);

            let get_result = tracking_copy
                .borrow_mut()
                .get(&highest_era_info_key)
                .map_err(|error| Error::Exec(error.into()))?;

            match get_result {
                Some(stored_value @ StoredValue::EraInfo(_)) => {
                    tracking_copy
                        .borrow_mut()
                        .write(Key::EraSummary, stored_value);
                }
                Some(other_stored_value) => {
                    // This should not happen as we only write EraInfo variants.
                    error!(stored_value_type_name=%other_stored_value.type_name(),
                        "EraInfo key contains unexpected StoredValue variant");
                    return Err(Error::ProtocolUpgrade(
                        ProtocolUpgradeError::UnexpectedStoredValueVariant,
                    ));
                }
                None => {
                    // Can't find key
                    // Most likely this chain did not yet ran an auction, or recently completed a
                    // prune
                }
            };
        }

        let effects = tracking_copy.borrow().effects();

        // commit
        let post_state_hash = self
            .state
            .commit(pre_state_hash, effects.clone())
            .map_err(Into::<Error>::into)?;

        // return result and effects
        Ok(UpgradeSuccess {
            post_state_hash,
            effects,
        })
    }

    /// Commit a prune of leaf nodes from the tip of the merkle trie.
    pub fn commit_prune(&self, prune_config: PruneConfig) -> Result<PruneResult, Error> {
        let pre_state_hash = prune_config.pre_state_hash();

        // Validate the state root hash just to make sure we can safely short circuit in case the
        // list of keys is empty.
        let tracking_copy = match self.tracking_copy(pre_state_hash)? {
            None => return Ok(PruneResult::RootNotFound),
            Some(tracking_copy) => Rc::new(RefCell::new(tracking_copy)),
        };

        let keys_to_delete = prune_config.keys_to_prune();
        if keys_to_delete.is_empty() {
            // effectively a noop
            return Ok(PruneResult::Success {
                post_state_hash: pre_state_hash,
                effects: Effects::default(),
            });
        }

        for key in keys_to_delete {
            tracking_copy.borrow_mut().prune(*key)
        }

        let effects = tracking_copy.borrow().effects();

        let post_state_hash = self
            .state
            .commit(pre_state_hash, effects.clone())
            .map_err(Into::<execution::Error>::into)?;

        Ok(PruneResult::Success {
            post_state_hash,
            effects,
        })
    }

    /// Creates a new tracking copy instance.
    pub fn tracking_copy(
        &self,
        hash: Digest,
    ) -> Result<Option<TrackingCopy<S::Reader>>, TrackingCopyError> {
        match self.state.checkout(hash) {
            Ok(ret) => match ret {
                Some(tc) => Ok(Some(TrackingCopy::new(tc, self.config.max_query_depth))),
                None => Ok(None),
            },
            Err(err) => Err(TrackingCopyError::Storage(err)),
        }
    }

    /// Executes a query.
    ///
    /// For a given root [`Key`] it does a path lookup through the named keys.
    ///
    /// Returns the value stored under a [`URef`] wrapped in a [`QueryResult`].
    pub fn run_query(&self, query_request: QueryRequest) -> QueryResult {
        let state_hash = query_request.state_hash();
        let query_key = query_request.key();
        let query_path = query_request.path();
        let query_result = match self.tracking_copy(state_hash) {
            Ok(Some(tc)) => match tc.query(query_key, query_path) {
                Ok(ret) => ret.into(),
                Err(err) => QueryResult::Failure(err),
            },
            Ok(None) => QueryResult::RootNotFound,
            Err(err) => QueryResult::Failure(err),
        };

        if let QueryResult::ValueNotFound(_) = query_result {
            if query_key.is_system_key() {
                if let Some(entity_addr) = query_key.into_entity_hash_addr() {
                    debug!("Compensating for AddressableEntity move");
                    let legacy_query_key = Key::Hash(entity_addr);
                    let legacy_request =
                        QueryRequest::new(state_hash, legacy_query_key, query_path.to_vec());
                    return self.run_query(legacy_request);
                }
            }
        }
        query_result
    }

    /// Runs a deploy execution request.
    ///
    /// For each deploy stored in the request it will execute it.
    ///
    /// Currently a special shortcut is taken to distinguish a native transfer, from a deploy.
    ///
    /// Return execution results which contains results from each deploy ran.
    pub fn run_execute(&self, mut exec_request: ExecuteRequest) -> Result<ExecutionResults, Error> {
        let executor = Executor::new(self.config().clone());

        let deploys = exec_request.take_deploys();
        let mut results = ExecutionResults::with_capacity(deploys.len());

        for deploy_item in deploys {
            let result = match deploy_item.session {
                ExecutableDeployItem::Transfer { .. } => self.transfer(
                    &executor,
                    exec_request.protocol_version,
                    exec_request.parent_state_hash,
                    BlockTime::new(exec_request.block_time),
                    deploy_item,
                    exec_request.proposer.clone(),
                ),
                _ => self.deploy(
                    &executor,
                    exec_request.protocol_version,
                    exec_request.parent_state_hash,
                    BlockTime::new(exec_request.block_time),
                    deploy_item,
                    exec_request.proposer.clone(),
                ),
            };
            match result {
                Ok(result) => results.push_back(result),
                Err(error) => {
                    return Err(error);
                }
            };
        }

        Ok(results)
    }

    fn get_authorized_addressable_entity(
        &self,
        protocol_version: ProtocolVersion,
        account_hash: AccountHash,
        authorization_keys: &BTreeSet<AccountHash>,
        tracking_copy: Rc<RefCell<TrackingCopy<<S as StateProvider>::Reader>>>,
    ) -> Result<(AddressableEntity, AddressableEntityHash), Error> {
        let entity_record = match tracking_copy
            .borrow_mut()
            .get_addressable_entity_by_account_hash(protocol_version, account_hash)
        {
            Ok(entity) => entity,
            Err(_) => return Err(Error::MissingContractByAccountHash(account_hash)),
        };

        let entity_hash: AddressableEntityHash = match tracking_copy
            .borrow_mut()
            .get_entity_hash_by_account_hash(account_hash)
        {
            Ok(contract_hash) => contract_hash,
            Err(error) => {
                return Err(error.into());
            }
        };

        let admin_set = self.config().administrative_accounts();

        if !admin_set.is_empty() && admin_set.intersection(authorization_keys).next().is_some() {
            // Exit early if there's at least a single signature coming from an admin.
            return Ok((entity_record, entity_hash));
        }

        // Authorize using provided authorization keys
        if !entity_record.can_authorize(authorization_keys) {
            return Err(Error::Authorization);
        }

        // Check total key weight against deploy threshold
        if !entity_record.can_deploy_with(authorization_keys) {
            return Err(execution::Error::DeploymentAuthorizationFailure.into());
        }

        Ok((entity_record, entity_hash))
    }

    fn create_addressable_entity_from_account(
        &self,
        account: Account,
        protocol_version: ProtocolVersion,
        tracking_copy: Rc<RefCell<TrackingCopy<<S as StateProvider>::Reader>>>,
    ) -> Result<(), Error> {
        let account_hash = account.account_hash();

        let mut generator =
            AddressGenerator::new(account.main_purse().addr().as_ref(), Phase::System);

        let contract_wasm_hash = *ACCOUNT_BYTE_CODE_HASH;
        let entity_hash = AddressableEntityHash::new(generator.new_hash_address());
        let package_hash = PackageHash::new(generator.new_hash_address());

        let entry_points = EntryPoints::new();

        let associated_keys = AssociatedKeys::from(account.associated_keys().clone());
        let action_thresholds = {
            let account_threshold = account.action_thresholds().clone();
            ActionThresholds::new(
                Weight::new(account_threshold.deployment.value()),
                Weight::new(1u8),
                Weight::new(account_threshold.key_management.value()),
            )
            .map_err(|_| Error::Authorization)?
        };

        let entity_addr = EntityAddr::new_account_entity_addr(entity_hash.value());

        self.migrate_named_keys(
            entity_addr,
            account.named_keys().clone(),
            Rc::clone(&tracking_copy),
        )?;

        let entity = AddressableEntity::new(
            package_hash,
            contract_wasm_hash,
            entry_points,
            protocol_version,
            account.main_purse(),
            associated_keys,
            action_thresholds,
            MessageTopics::default(),
            EntityKind::Account(account_hash),
        );

        let access_key = generator.new_uref(AccessRights::READ_ADD_WRITE);

        let package = {
            let mut package = Package::new(
                access_key,
                EntityVersions::default(),
                BTreeSet::default(),
                Groups::default(),
                PackageStatus::Locked,
            );
            package.insert_entity_version(protocol_version.value().major, entity_hash);
            package
        };

        let entity_key: Key = entity.entity_key(entity_hash);

        tracking_copy.borrow_mut().write(entity_key, entity.into());
        tracking_copy
            .borrow_mut()
            .write(package_hash.into(), package.into());
        let contract_by_account = match CLValue::from_t(entity_key) {
            Ok(cl_value) => cl_value,
            Err(_) => return Err(Error::Bytesrepr("Failed to convert to CLValue".to_string())),
        };

        tracking_copy.borrow_mut().write(
            Key::Account(account_hash),
            StoredValue::CLValue(contract_by_account),
        );
        Ok(())
    }

    fn migrate_account(
        &self,
        account_hash: AccountHash,
        protocol_version: ProtocolVersion,
        tracking_copy: Rc<RefCell<TrackingCopy<<S as StateProvider>::Reader>>>,
    ) -> Result<(), Error> {
        let maybe_stored_value = tracking_copy
            .borrow_mut()
            .read(&Key::Account(account_hash))
            .map_err(Into::<Error>::into)?;

        match maybe_stored_value {
            Some(StoredValue::Account(account)) => self.create_addressable_entity_from_account(
                account,
                protocol_version,
                Rc::clone(&tracking_copy),
            ),
            Some(StoredValue::CLValue(_)) => Ok(()),
            // This means the Account does not exist, which we consider to be
            // an authorization error. As used by the node, this type of deploy
            // will have already been filtered out, but for other EE use cases
            // and testing it is reachable.
            Some(_) | None => Err(Error::Authorization),
        }
    }

    fn migrate_named_keys(
        &self,
        entity_addr: EntityAddr,
        named_keys: NamedKeys,
        tracking_copy: Rc<RefCell<TrackingCopy<<S as StateProvider>::Reader>>>,
    ) -> Result<(), Error> {
        for (string, key) in named_keys.into_inner().into_iter() {
            let entry_addr = NamedKeyAddr::new_from_string(entity_addr, string.clone())
                .map_err(|error| Error::Bytesrepr(error.to_string()))?;

            let named_key_value = StoredValue::NamedKey(
                NamedKeyValue::from_concrete_values(key, string.clone())
                    .map_err(|cl_error| Error::Bytesrepr(cl_error.to_string()))?,
            );

            let entry_key = Key::NamedKey(entry_addr);

            tracking_copy.borrow_mut().write(entry_key, named_key_value)
        }

        Ok(())
    }

    fn get_named_keys(
        &self,
        entity_addr: EntityAddr,
        tracking_copy: Rc<RefCell<TrackingCopy<<S as StateProvider>::Reader>>>,
    ) -> Result<NamedKeys, Error> {
        tracking_copy
            .borrow_mut()
            .get_named_keys(entity_addr)
            .map_err(Into::into)
    }

    /// Get the balance of a passed purse referenced by its [`URef`].
    pub fn get_purse_balance(
        &self,
        state_hash: Digest,
        purse_uref: URef,
    ) -> Result<BalanceResult, Error> {
        let tracking_copy = match self.tracking_copy(state_hash)? {
            Some(tracking_copy) => tracking_copy,
            None => return Ok(BalanceResult::RootNotFound),
        };
        let purse_balance_key = tracking_copy.get_purse_balance_key(purse_uref.into())?;
        let (balance, proof) = tracking_copy.get_purse_balance_with_proof(purse_balance_key)?;
        let proof = Box::new(proof);
        let motes = balance.value();
        Ok(BalanceResult::Success { motes, proof })
    }

    /// Executes a native transfer.
    ///
    /// Native transfers do not involve WASM at all, and also skip executing payment code.
    /// Therefore this is the fastest and cheapest way to transfer tokens from account to account.
    ///
    /// Returns an [`ExecutionResult`] for a successful native transfer.
    #[allow(clippy::too_many_arguments)]
    pub fn transfer(
        &self,
        executor: &Executor,
        protocol_version: ProtocolVersion,
        prestate_hash: Digest,
        blocktime: BlockTime,
        deploy_item: DeployItem,
        proposer: PublicKey,
    ) -> Result<ExecutionResult, Error> {
        let tracking_copy = match self.tracking_copy(prestate_hash) {
            Err(tce) => {
                return Ok(ExecutionResult::precondition_failure(Error::TrackingCopy(
                    tce,
                )))
            }
            Ok(None) => return Err(Error::RootNotFound(prestate_hash)),
            Ok(Some(tracking_copy)) => Rc::new(RefCell::new(tracking_copy)),
        };

        let account_hash = deploy_item.address;

        let authorization_keys = deploy_item.authorization_keys;

        // Migrate the legacy account structure if necessary.
        if let Err(e) =
            self.migrate_account(account_hash, protocol_version, Rc::clone(&tracking_copy))
        {
            return Ok(ExecutionResult::precondition_failure(e));
        }

        let (entity, entity_hash) = match self.get_authorized_addressable_entity(
            protocol_version,
            account_hash,
            &authorization_keys,
            Rc::clone(&tracking_copy),
        ) {
            Ok(account) => account,
            Err(e) => return Ok(ExecutionResult::precondition_failure(e)),
        };

        let package_kind = entity.entity_kind();

        let entity_addr = EntityAddr::new_with_tag(package_kind, entity_hash.value());

        let entity_named_keys = match self.get_named_keys(entity_addr, Rc::clone(&tracking_copy)) {
            Ok(named_keys) => named_keys,
            Err(error) => {
                return Ok(ExecutionResult::precondition_failure(error));
            }
        };

        let system_contract_registry = tracking_copy.borrow_mut().get_system_contracts()?;

        let handle_payment_contract_hash = system_contract_registry
            .get(HANDLE_PAYMENT)
            .ok_or_else(|| {
                error!("Missing system handle payment contract hash");
                Error::MissingSystemContractHash(HANDLE_PAYMENT.to_string())
            })?;

        let handle_payment_contract = match tracking_copy
            .borrow_mut()
            .get_addressable_entity(*handle_payment_contract_hash)
        {
            Ok(contract) => contract,
            Err(error) => {
                return Ok(ExecutionResult::precondition_failure(error.into()));
            }
        };

        let handle_payment_addr =
            EntityAddr::new_system_entity_addr(handle_payment_contract_hash.value());

        let handle_payment_named_keys =
            match self.get_named_keys(handle_payment_addr, Rc::clone(&tracking_copy)) {
                Ok(named_keys) => named_keys,
                Err(error) => {
                    return Ok(ExecutionResult::precondition_failure(error));
                }
            };

        let mut handle_payment_access_rights = handle_payment_contract
            .extract_access_rights(*handle_payment_contract_hash, &handle_payment_named_keys);

        let gas_limit = Gas::new(U512::from(std::u64::MAX));

        let wasmless_transfer_gas_cost = Gas::new(U512::from(
            self.config().system_config().wasmless_transfer_cost(),
        ));

        let wasmless_transfer_motes = match Motes::from_gas(
            wasmless_transfer_gas_cost,
            WASMLESS_TRANSFER_FIXED_GAS_PRICE,
        ) {
            Some(motes) => motes,
            None => {
                return Ok(ExecutionResult::precondition_failure(
                    Error::GasConversionOverflow,
                ))
            }
        };

        let rewards_target_purse =
            match self.get_rewards_purse(protocol_version, proposer, prestate_hash) {
                Ok(target_purse) => target_purse,
                Err(error) => return Ok(ExecutionResult::precondition_failure(error)),
            };

        let rewards_target_purse_balance_key = {
            match tracking_copy
                .borrow_mut()
                .get_purse_balance_key(rewards_target_purse.into())
            {
                Ok(balance_key) => balance_key,
                Err(error) => {
                    return Ok(ExecutionResult::precondition_failure(Error::TrackingCopy(
                        error,
                    )))
                }
            }
        };

        let account_main_purse = entity.main_purse();

        let account_main_purse_balance_key = match tracking_copy
            .borrow_mut()
            .get_purse_balance_key(account_main_purse.into())
        {
            Ok(balance_key) => balance_key,
            Err(error) => {
                return Ok(ExecutionResult::precondition_failure(Error::TrackingCopy(
                    error,
                )))
            }
        };

        let account_main_purse_balance = match tracking_copy
            .borrow_mut()
            .get_purse_balance(account_main_purse_balance_key)
        {
            Ok(balance_key) => balance_key,
            Err(error) => {
                return Ok(ExecutionResult::precondition_failure(Error::TrackingCopy(
                    error,
                )))
            }
        };

        if account_main_purse_balance < wasmless_transfer_motes {
            // We don't have minimum balance to operate and therefore we can't charge for user
            // errors.
            return Ok(ExecutionResult::precondition_failure(
                Error::InsufficientPayment,
            ));
        }

        // Function below creates an ExecutionResult with precomputed effects of "finalize_payment".
        let make_charged_execution_failure = |error| match ExecutionResult::new_payment_code_error(
            error,
            wasmless_transfer_motes,
            account_main_purse_balance,
            wasmless_transfer_gas_cost,
            account_main_purse_balance_key,
            rewards_target_purse_balance_key,
        ) {
            Ok(execution_result) => execution_result,
            Err(error) => ExecutionResult::precondition_failure(error),
        };

        // All wasmless transfer preconditions are met.
        // Any error that occurs in logic below this point would result in a charge for user error.
        let mut runtime_args_builder =
            TransferRuntimeArgsBuilder::new(deploy_item.session.args().clone());

        let transfer_target_mode = match runtime_args_builder
            .resolve_transfer_target_mode(protocol_version, Rc::clone(&tracking_copy))
        {
            Ok(transfer_target_mode) => transfer_target_mode,
            Err(error) => return Ok(make_charged_execution_failure(error)),
        };

        // At this point we know target refers to either a purse on an existing account or an
        // account which has to be created.

        if !self.config.allow_unrestricted_transfers()
            && !self.config.is_administrator(&account_hash)
        {
            // We need to make sure that source or target has to be admin.
            match transfer_target_mode {
                NewTransferTargetMode::ExistingAccount {
                    target_account_hash,
                    ..
                }
                | NewTransferTargetMode::CreateAccount(target_account_hash) => {
                    let is_target_system_account =
                        target_account_hash == PublicKey::System.to_account_hash();
                    let is_target_administrator =
                        self.config.is_administrator(&target_account_hash);
                    if !(is_target_system_account || is_target_administrator) {
                        // Transferring from normal account to a purse doesn't work.
                        return Ok(make_charged_execution_failure(
                            execution::Error::DisabledUnrestrictedTransfers.into(),
                        ));
                    }
                }
                NewTransferTargetMode::PurseExists(_) => {
                    // We don't know who is the target and we can't simply reverse search
                    // account/contract that owns it. We also can't know if purse is owned exactly
                    // by one entity in the system.
                    return Ok(make_charged_execution_failure(
                        execution::Error::DisabledUnrestrictedTransfers.into(),
                    ));
                }
            }
        }

        match transfer_target_mode {
            NewTransferTargetMode::ExistingAccount { .. }
            | NewTransferTargetMode::PurseExists(_) => {
                // Noop
            }
            NewTransferTargetMode::CreateAccount(account_hash) => {
                let create_purse_stack = self.get_new_system_call_stack();

                let (maybe_uref, execution_result): (Option<URef>, ExecutionResult) = executor
                    .call_system_contract(
                        DirectSystemContractCall::CreatePurse,
                        RuntimeArgs::new(), // mint create takes no arguments
                        &entity,
                        package_kind,
                        authorization_keys.clone(),
                        account_hash,
                        blocktime,
                        deploy_item.deploy_hash,
                        gas_limit,
                        protocol_version,
                        Rc::clone(&tracking_copy),
                        Phase::Session,
                        create_purse_stack,
                        // We're just creating a purse.
                        U512::zero(),
                    );
                match maybe_uref {
                    Some(main_purse) => {
                        let account = Account::create(account_hash, NamedKeys::new(), main_purse);
                        if let Err(error) = self.create_addressable_entity_from_account(
                            account,
                            protocol_version,
                            Rc::clone(&tracking_copy),
                        ) {
                            return Ok(make_charged_execution_failure(error));
                        }
                    }
                    None => {
                        // This case implies that the execution_result is a failure variant as
                        // implemented inside host_exec().
                        let error = execution_result
                            .take_error()
                            .unwrap_or(Error::InsufficientPayment);
                        return Ok(make_charged_execution_failure(error));
                    }
                }
            }
        }

        let transfer_args = match runtime_args_builder.build(
            &entity,
            entity_named_keys,
            protocol_version,
            Rc::clone(&tracking_copy),
        ) {
            Ok(transfer_args) => transfer_args,
            Err(error) => return Ok(make_charged_execution_failure(error)),
        };

        let payment_uref;

        // Construct a payment code that will put cost of wasmless payment into payment purse
        let payment_result = {
            // Check source purses minimum balance
            let source_uref = transfer_args.source();
            let source_purse_balance = if source_uref != account_main_purse {
                let source_purse_balance_key = match tracking_copy
                    .borrow_mut()
                    .get_purse_balance_key(Key::URef(source_uref))
                {
                    Ok(purse_balance_key) => purse_balance_key,
                    Err(error) => {
                        return Ok(make_charged_execution_failure(Error::TrackingCopy(error)))
                    }
                };

                match tracking_copy
                    .borrow_mut()
                    .get_purse_balance(source_purse_balance_key)
                {
                    Ok(purse_balance) => purse_balance,
                    Err(error) => {
                        return Ok(make_charged_execution_failure(Error::TrackingCopy(error)))
                    }
                }
            } else {
                // If source purse is main purse then we already have the balance.
                account_main_purse_balance
            };

            let transfer_amount_motes = Motes::new(transfer_args.amount());

            match wasmless_transfer_motes.checked_add(transfer_amount_motes) {
                Some(total_amount) if source_purse_balance < total_amount => {
                    // We can't continue if the minimum funds in source purse are lower than the
                    // required cost.
                    return Ok(make_charged_execution_failure(Error::InsufficientPayment));
                }
                None => {
                    // When trying to send too much that could cause an overflow.
                    return Ok(make_charged_execution_failure(Error::InsufficientPayment));
                }
                Some(_) => {}
            }

            let get_payment_purse_stack = self.get_new_system_call_stack();
            let (maybe_payment_uref, get_payment_purse_result): (Option<URef>, ExecutionResult) =
                executor.call_system_contract(
                    DirectSystemContractCall::GetPaymentPurse,
                    RuntimeArgs::default(),
                    &entity,
                    package_kind,
                    authorization_keys.clone(),
                    account_hash,
                    blocktime,
                    deploy_item.deploy_hash,
                    gas_limit,
                    protocol_version,
                    Rc::clone(&tracking_copy),
                    Phase::Payment,
                    get_payment_purse_stack,
                    // Getting payment purse does not require transfering tokens.
                    U512::zero(),
                );

            payment_uref = match maybe_payment_uref {
                Some(payment_uref) => payment_uref,
                None => return Ok(make_charged_execution_failure(Error::InsufficientPayment)),
            };

            if let Some(error) = get_payment_purse_result.take_error() {
                return Ok(make_charged_execution_failure(error));
            }

            // Create a new arguments to transfer cost of wasmless transfer into the payment purse.

            let new_transfer_args = TransferArgs::new(
                transfer_args.to(),
                transfer_args.source(),
                payment_uref,
                wasmless_transfer_motes.value(),
                transfer_args.arg_id(),
            );

            let runtime_args = match RuntimeArgs::try_from(new_transfer_args) {
                Ok(runtime_args) => runtime_args,
                Err(error) => return Ok(make_charged_execution_failure(Error::Exec(error.into()))),
            };

            let transfer_to_payment_purse_stack = self.get_new_system_call_stack();
            let (actual_result, payment_result): (Option<Result<(), u8>>, ExecutionResult) =
                executor.call_system_contract(
                    DirectSystemContractCall::Transfer,
                    runtime_args,
                    &entity,
                    package_kind,
                    authorization_keys.clone(),
                    account_hash,
                    blocktime,
                    deploy_item.deploy_hash,
                    gas_limit,
                    protocol_version,
                    Rc::clone(&tracking_copy),
                    Phase::Payment,
                    transfer_to_payment_purse_stack,
                    // We should use only as much as transfer costs.
                    // We're not changing the allowed spending limit since this is a system cost.
                    wasmless_transfer_motes.value(),
                );

            if let Some(error) = payment_result.as_error().cloned() {
                return Ok(make_charged_execution_failure(error));
            }

            let transfer_result = match actual_result {
                Some(Ok(())) => Ok(()),
                Some(Err(mint_error)) => match mint::Error::try_from(mint_error) {
                    Ok(mint_error) => Err(ApiError::from(mint_error)),
                    Err(_) => Err(ApiError::Transfer),
                },
                None => Err(ApiError::Transfer),
            };

            if let Err(error) = transfer_result {
                return Ok(make_charged_execution_failure(Error::Exec(
                    ExecError::Revert(error),
                )));
            }

            let payment_purse_balance = {
                let payment_purse_balance_key = match tracking_copy
                    .borrow_mut()
                    .get_purse_balance_key(Key::URef(payment_uref))
                {
                    Ok(payment_purse_balance_key) => payment_purse_balance_key,
                    Err(error) => {
                        return Ok(make_charged_execution_failure(Error::TrackingCopy(error)))
                    }
                };

                match tracking_copy
                    .borrow_mut()
                    .get_purse_balance(payment_purse_balance_key)
                {
                    Ok(payment_purse_balance) => payment_purse_balance,
                    Err(error) => {
                        return Ok(make_charged_execution_failure(Error::TrackingCopy(error)))
                    }
                }
            };

            // Wasmless transfer payment code pre & post conditions:
            // (a) payment purse should be empty before the payment operation
            // (b) after executing payment code it's balance has to be equal to the wasmless gas
            // cost price

            let payment_gas =
                match Gas::from_motes(payment_purse_balance, WASMLESS_TRANSFER_FIXED_GAS_PRICE) {
                    Some(gas) => gas,
                    None => {
                        return Ok(make_charged_execution_failure(Error::GasConversionOverflow))
                    }
                };

            debug_assert_eq!(payment_gas, wasmless_transfer_gas_cost);

            // This assumes the cost incurred is already denominated in gas

            payment_result.with_cost(payment_gas)
        };

        let runtime_args = match RuntimeArgs::try_from(transfer_args) {
            Ok(runtime_args) => runtime_args,
            Err(error) => {
                return Ok(make_charged_execution_failure(
                    ExecError::from(error).into(),
                ))
            }
        };

        let transfer_stack = self.get_new_system_call_stack();
        let (_, mut session_result): (Option<Result<(), u8>>, ExecutionResult) = executor
            .call_system_contract(
                DirectSystemContractCall::Transfer,
                runtime_args,
                &entity,
                package_kind,
                authorization_keys.clone(),
                account_hash,
                blocktime,
                deploy_item.deploy_hash,
                gas_limit,
                protocol_version,
                Rc::clone(&tracking_copy),
                Phase::Session,
                transfer_stack,
                // We limit native transfer to the amount that user signed over as `amount`
                // argument.
                transfer_args.amount(),
            );

        // User is already charged fee for wasmless contract, and we need to make sure we will not
        // charge for anything that happens while calling transfer entrypoint.
        session_result = session_result.with_cost(Gas::default());

        let finalize_result = {
            let handle_payment_args = {
                // Gas spent during payment code execution
                let finalize_cost_motes = {
                    // A case where payment_result.cost() is different than wasmless transfer cost
                    // is considered a programming error.
                    debug_assert_eq!(payment_result.cost(), wasmless_transfer_gas_cost);
                    wasmless_transfer_motes
                };

                let account = deploy_item.address;
                let maybe_runtime_args = RuntimeArgs::try_new(|args| {
                    args.insert(handle_payment::ARG_AMOUNT, finalize_cost_motes.value())?;
                    args.insert(handle_payment::ARG_ACCOUNT, account)?;
                    args.insert(handle_payment::ARG_TARGET, rewards_target_purse)?;
                    Ok(())
                });

                match maybe_runtime_args {
                    Ok(runtime_args) => runtime_args,
                    Err(error) => {
                        let exec_error = ExecError::from(error);
                        return Ok(ExecutionResult::precondition_failure(exec_error.into()));
                    }
                }
            };

            let system_addressable_entity = {
                tracking_copy
                    .borrow_mut()
                    .get_addressable_entity_by_account_hash(
                        protocol_version,
                        PublicKey::System.to_account_hash(),
                    )?
            };

            let tc = tracking_copy.borrow();
            let finalization_tc = Rc::new(RefCell::new(tc.fork()));

            let finalize_payment_stack = self.get_new_system_call_stack();
            handle_payment_access_rights.extend(&[payment_uref, rewards_target_purse]);

            let (_ret, finalize_result): (Option<()>, ExecutionResult) = executor
                .call_system_contract(
                    DirectSystemContractCall::FinalizePayment,
                    handle_payment_args,
                    &system_addressable_entity,
                    EntityKind::Account(PublicKey::System.to_account_hash()),
                    authorization_keys,
                    PublicKey::System.to_account_hash(),
                    blocktime,
                    deploy_item.deploy_hash,
                    gas_limit,
                    protocol_version,
                    finalization_tc,
                    Phase::FinalizePayment,
                    finalize_payment_stack,
                    // Spending limit is cost of wasmless execution.
                    U512::from(self.config().system_config().wasmless_transfer_cost()),
                );

            finalize_result
        };

        // Create + persist deploy info.
        {
            let transfers = session_result.transfers();
            let cost = wasmless_transfer_gas_cost.value();
            let deploy_info = DeployInfo::new(
                deploy_item.deploy_hash,
                transfers,
                account_hash,
                entity.main_purse(),
                cost,
            );
            tracking_copy.borrow_mut().write(
                Key::DeployInfo(deploy_item.deploy_hash),
                StoredValue::DeployInfo(deploy_info),
            );
        }

        if session_result.is_success() {
            session_result = session_result.with_effects(tracking_copy.borrow().effects())
        }

        let mut execution_result_builder = ExecutionResultBuilder::new();
        execution_result_builder.set_payment_execution_result(payment_result);
        execution_result_builder.set_session_execution_result(session_result);
        execution_result_builder.set_finalize_execution_result(finalize_result);

        let execution_result = execution_result_builder
            .build()
            .expect("ExecutionResultBuilder not initialized properly");

        Ok(execution_result)
    }

    /// Executes a deploy.
    ///
    /// A deploy execution consists of running the payment code, which is expected to deposit funds
    /// into the payment purse, and then running the session code with a specific gas limit. For
    /// running payment code, we lock [`MAX_PAYMENT`] amount of motes from the user as collateral.
    /// If both the payment code and the session code execute successfully, a fraction of the
    /// unspent collateral will be transferred back to the proposer of the deploy, as specified
    /// in the request.
    ///
    /// Returns [`ExecutionResult`], or an error condition.
    #[allow(clippy::too_many_arguments)]
    pub fn deploy(
        &self,
        executor: &Executor,
        protocol_version: ProtocolVersion,
        prestate_hash: Digest,
        blocktime: BlockTime,
        deploy_item: DeployItem,
        proposer: PublicKey,
    ) -> Result<ExecutionResult, Error> {
        // spec: https://casperlabs.atlassian.net/wiki/spaces/EN/pages/123404576/Payment+code+execution+specification

        // Create tracking copy (which functions as a deploy context)
        // validation_spec_2: prestate_hash check
        // do this second; as there is no reason to proceed if the prestate hash is invalid
        let tracking_copy = match self.tracking_copy(prestate_hash) {
            Err(tce) => {
                return Ok(ExecutionResult::precondition_failure(Error::TrackingCopy(
                    tce,
                )))
            }
            Ok(None) => return Err(Error::RootNotFound(prestate_hash)),
            Ok(Some(tracking_copy)) => Rc::new(RefCell::new(tracking_copy)),
        };

        // Get addr bytes from `address` (which is actually a Key)
        // validation_spec_3: account validity

        let authorization_keys = deploy_item.authorization_keys;
        let account_hash = deploy_item.address;

        if let Err(error) =
            self.migrate_account(account_hash, protocol_version, Rc::clone(&tracking_copy))
        {
            return Ok(ExecutionResult::precondition_failure(error));
        }

        // Get account from tracking copy
        // validation_spec_3: account validity
        let (entity, entity_hash) = {
            match self.get_authorized_addressable_entity(
                protocol_version,
                account_hash,
                &authorization_keys,
                Rc::clone(&tracking_copy),
            ) {
                Ok((addressable_entity, entity_hash)) => (addressable_entity, entity_hash),
                Err(e) => return Ok(ExecutionResult::precondition_failure(e)),
            }
        };

        let entity_kind = entity.entity_kind();

        let entity_addr = EntityAddr::new_with_tag(entity_kind, entity_hash.value());

        let entity_named_keys = match self.get_named_keys(entity_addr, Rc::clone(&tracking_copy)) {
            Ok(named_keys) => named_keys,
            Err(error) => {
                return Ok(ExecutionResult::precondition_failure(error));
            }
        };

        let payment = deploy_item.payment;
        let session = deploy_item.session;

        let deploy_hash = deploy_item.deploy_hash;

        let session_args = session.args().clone();

        // Create session code `A` from provided session bytes
        // validation_spec_1: valid wasm bytes
        // we do this upfront as there is no reason to continue if session logic is invalid
        let session_execution_kind = match ExecutionKind::new(
            Rc::clone(&tracking_copy),
            &entity_named_keys,
            session,
            &protocol_version,
            Phase::Session,
        ) {
            Ok(execution_kind) => execution_kind,
            Err(error) => {
                return Ok(ExecutionResult::precondition_failure(error));
            }
        };

        // Get account main purse balance key
        // validation_spec_5: account main purse minimum balance
        let entity_main_purse_key: Key = {
            let account_key = Key::URef(entity.main_purse());
            match tracking_copy
                .borrow_mut()
                .get_purse_balance_key(account_key)
            {
                Ok(key) => key,
                Err(error) => {
                    return Ok(ExecutionResult::precondition_failure(error.into()));
                }
            }
        };

        // Get account main purse balance to enforce precondition and in case of forced
        // transfer validation_spec_5: account main purse minimum balance
        let account_main_purse_balance: Motes = match tracking_copy
            .borrow_mut()
            .get_purse_balance(entity_main_purse_key)
        {
            Ok(balance) => balance,
            Err(error) => return Ok(ExecutionResult::precondition_failure(error.into())),
        };

        let max_payment_cost = Motes::new(*MAX_PAYMENT);

        // Enforce minimum main purse balance validation
        // validation_spec_5: account main purse minimum balance
        if account_main_purse_balance < max_payment_cost {
            return Ok(ExecutionResult::precondition_failure(
                Error::InsufficientPayment,
            ));
        }

        // Finalization is executed by system account (currently genesis account)
        // payment_code_spec_5: system executes finalization
        let system_addressable_entity = tracking_copy
            .borrow_mut()
            .read_addressable_entity_by_account_hash(
                protocol_version,
                PublicKey::System.to_account_hash(),
            )?;

        // Get handle payment system contract details
        // payment_code_spec_6: system contract validity
        let system_contract_registry = tracking_copy.borrow_mut().get_system_contracts()?;

        let handle_payment_contract_hash = system_contract_registry
            .get(HANDLE_PAYMENT)
            .ok_or_else(|| {
                error!("Missing system handle payment contract hash");
                Error::MissingSystemContractHash(HANDLE_PAYMENT.to_string())
            })?;

        let handle_payment_addr =
            EntityAddr::new_system_entity_addr(handle_payment_contract_hash.value());

        let handle_payment_named_keys =
            match self.get_named_keys(handle_payment_addr, Rc::clone(&tracking_copy)) {
                Ok(named_keys) => named_keys,
                Err(error) => {
                    return Ok(ExecutionResult::precondition_failure(error));
                }
            };

        // Get payment purse Key from handle payment contract
        // payment_code_spec_6: system contract validity
        let payment_purse_key =
            match handle_payment_named_keys.get(handle_payment::PAYMENT_PURSE_KEY) {
                Some(key) => *key,
                None => return Ok(ExecutionResult::precondition_failure(Error::Deploy)),
            };

        let payment_purse_uref = payment_purse_key
            .into_uref()
            .ok_or(Error::InvalidKeyVariant)?;

        // [`ExecutionResultBuilder`] handles merging of multiple execution results
        let mut execution_result_builder = execution_result::ExecutionResultBuilder::new();

        let rewards_target_purse =
            match self.get_rewards_purse(protocol_version, proposer, prestate_hash) {
                Ok(target_purse) => target_purse,
                Err(error) => return Ok(ExecutionResult::precondition_failure(error)),
            };

        let rewards_target_purse_balance_key = {
            // Get reward purse Key from handle payment contract
            // payment_code_spec_6: system contract validity
            match tracking_copy
                .borrow_mut()
                .get_purse_balance_key(rewards_target_purse.into())
            {
                Ok(key) => key,
                Err(error) => {
                    return Ok(ExecutionResult::precondition_failure(error.into()));
                }
            }
        };

        // Execute provided payment code
        let payment_result = {
            // payment_code_spec_1: init pay environment w/ gas limit == (max_payment_cost /
            // gas_price)
            let payment_gas_limit = match Gas::from_motes(max_payment_cost, deploy_item.gas_price) {
                Some(gas) => gas,
                None => {
                    return Ok(ExecutionResult::precondition_failure(
                        Error::GasConversionOverflow,
                    ))
                }
            };

            // Create payment code module from bytes
            // validation_spec_1: valid wasm bytes
            let phase = Phase::Payment;

            let payment_stack = RuntimeStack::from_account_hash(
                deploy_item.address,
                self.config.max_runtime_call_stack_height() as usize,
            );

            // payment_code_spec_2: execute payment code
            let payment_access_rights =
                entity.extract_access_rights(entity_hash, &entity_named_keys);

            let mut payment_named_keys = entity_named_keys.clone();

            let payment_args = payment.args().clone();

            if payment.is_standard_payment(phase) {
                // Todo potentially could be moved to Executor::Exec
                match executor.exec_standard_payment(
                    payment_args,
                    &entity,
                    entity_kind,
                    authorization_keys.clone(),
                    account_hash,
                    blocktime,
                    deploy_hash,
                    payment_gas_limit,
                    protocol_version,
                    Rc::clone(&tracking_copy),
                    self.config.max_runtime_call_stack_height() as usize,
                ) {
                    Ok(payment_result) => payment_result,
                    Err(error) => {
                        return Ok(ExecutionResult::precondition_failure(error));
                    }
                }
            } else {
                let payment_execution_kind = match ExecutionKind::new(
                    Rc::clone(&tracking_copy),
                    &entity_named_keys,
                    payment,
                    &protocol_version,
                    phase,
                ) {
                    Ok(execution_kind) => execution_kind,
                    Err(error) => {
                        return Ok(ExecutionResult::precondition_failure(error));
                    }
                };
                executor.exec(
                    payment_execution_kind,
                    payment_args,
                    entity_hash,
                    &entity,
                    entity_kind,
                    &mut payment_named_keys,
                    payment_access_rights,
                    authorization_keys.clone(),
                    account_hash,
                    blocktime,
                    deploy_hash,
                    payment_gas_limit,
                    protocol_version,
                    Rc::clone(&tracking_copy),
                    phase,
                    payment_stack,
                )
            }
        };
        log_execution_result("payment result", &payment_result);

        // If provided wasm file was malformed, we should charge.
        if should_charge_for_errors_in_wasm(&payment_result) {
            let error = payment_result
                .as_error()
                .cloned()
                .unwrap_or(Error::InsufficientPayment);

            match ExecutionResult::new_payment_code_error(
                error,
                max_payment_cost,
                account_main_purse_balance,
                payment_result.cost(),
                entity_main_purse_key,
                rewards_target_purse_balance_key,
            ) {
                Ok(execution_result) => return Ok(execution_result),
                Err(error) => return Ok(ExecutionResult::precondition_failure(error)),
            }
        }

        let payment_result_cost = payment_result.cost();
        // payment_code_spec_3: fork based upon payment purse balance and cost of
        // payment code execution

        // Get handle payment system contract details
        // payment_code_spec_6: system contract validity
        let system_contract_registry = tracking_copy.borrow_mut().get_system_contracts()?;

        let handle_payment_contract_hash = system_contract_registry
            .get(HANDLE_PAYMENT)
            .ok_or_else(|| {
                error!("Missing system handle payment contract hash");
                Error::MissingSystemContractHash(HANDLE_PAYMENT.to_string())
            })?;

        let handle_payment_addr =
            EntityAddr::new_system_entity_addr(handle_payment_contract_hash.value());

        let handle_payment_named_keys =
            match self.get_named_keys(handle_payment_addr, Rc::clone(&tracking_copy)) {
                Ok(named_keys) => named_keys,
                Err(error) => {
                    return Ok(ExecutionResult::precondition_failure(error));
                }
            };

        // Get payment purse Key from handle payment contract
        // payment_code_spec_6: system contract validity
        let payment_purse_key: Key =
            match handle_payment_named_keys.get(handle_payment::PAYMENT_PURSE_KEY) {
                Some(key) => *key,
                None => return Ok(ExecutionResult::precondition_failure(Error::Deploy)),
            };
        let purse_balance_key = match tracking_copy
            .borrow_mut()
            .get_purse_balance_key(payment_purse_key)
        {
            Ok(key) => key,
            Err(error) => {
                return Ok(ExecutionResult::precondition_failure(error.into()));
            }
        };
        let payment_purse_balance: Motes = {
            match tracking_copy
                .borrow_mut()
                .get_purse_balance(purse_balance_key)
            {
                Ok(balance) => balance,
                Err(error) => {
                    return Ok(ExecutionResult::precondition_failure(error.into()));
                }
            }
        };

        if let Some(forced_transfer) =
            payment_result.check_forced_transfer(payment_purse_balance, deploy_item.gas_price)
        {
            // Get rewards purse balance key
            // payment_code_spec_6: system contract validity
            let error = match forced_transfer {
                ForcedTransferResult::InsufficientPayment => Error::InsufficientPayment,
                ForcedTransferResult::GasConversionOverflow => Error::GasConversionOverflow,
                ForcedTransferResult::PaymentFailure => payment_result
                    .take_error()
                    .unwrap_or(Error::InsufficientPayment),
            };

            let gas_cost = match Gas::from_motes(max_payment_cost, deploy_item.gas_price) {
                Some(gas) => gas,
                None => {
                    return Ok(ExecutionResult::precondition_failure(
                        Error::GasConversionOverflow,
                    ))
                }
            };

            match ExecutionResult::new_payment_code_error(
                error,
                max_payment_cost,
                account_main_purse_balance,
                gas_cost,
                entity_main_purse_key,
                rewards_target_purse_balance_key,
            ) {
                Ok(execution_result) => return Ok(execution_result),
                Err(error) => return Ok(ExecutionResult::precondition_failure(error)),
            }
        };

        // Transfer the contents of the rewards purse to block proposer
        execution_result_builder.set_payment_execution_result(payment_result);

        // Begin session logic handling
        let post_payment_tracking_copy = tracking_copy.borrow();
        let session_tracking_copy = Rc::new(RefCell::new(post_payment_tracking_copy.fork()));

        let session_stack = RuntimeStack::from_account_hash(
            deploy_item.address,
            self.config.max_runtime_call_stack_height() as usize,
        );

        let mut session_named_keys = entity_named_keys.clone();

        let session_access_rights = entity.extract_access_rights(entity_hash, &entity_named_keys);

        let mut session_result = {
            // payment_code_spec_3_b_i: if (balance of handle payment pay purse) >= (gas spent
            // during payment code execution) * gas_price, yes session
            // session_code_spec_1: gas limit = ((balance of handle payment payment purse) /
            // gas_price)
            // - (gas spent during payment execution)
            let session_gas_limit: Gas =
                match Gas::from_motes(payment_purse_balance, deploy_item.gas_price)
                    .and_then(|gas| gas.checked_sub(payment_result_cost))
                {
                    Some(gas) => gas,
                    None => {
                        return Ok(ExecutionResult::precondition_failure(
                            Error::GasConversionOverflow,
                        ))
                    }
                };

            executor.exec(
                session_execution_kind,
                session_args,
                entity_hash,
                &entity,
                entity_kind,
                &mut session_named_keys,
                session_access_rights,
                authorization_keys.clone(),
                account_hash,
                blocktime,
                deploy_hash,
                session_gas_limit,
                protocol_version,
                Rc::clone(&session_tracking_copy),
                Phase::Session,
                session_stack,
            )
        };
        log_execution_result("session result", &session_result);

        // Create + persist deploy info.
        {
            let transfers = session_result.transfers();
            let cost = payment_result_cost.value() + session_result.cost().value();
            let deploy_info = DeployInfo::new(
                deploy_hash,
                transfers,
                account_hash,
                entity.main_purse(),
                cost,
            );
            session_tracking_copy.borrow_mut().write(
                Key::DeployInfo(deploy_hash),
                StoredValue::DeployInfo(deploy_info),
            );
        }

        // Session execution was zero cost or provided wasm was malformed.
        // Check if the payment purse can cover the minimum floor for session execution.
        if (session_result.cost().is_zero() && payment_purse_balance < max_payment_cost)
            || should_charge_for_errors_in_wasm(&session_result)
        {
            // When session code structure is valid but still has 0 cost we should propagate the
            // error.
            let error = session_result
                .as_error()
                .cloned()
                .unwrap_or(Error::InsufficientPayment);

            match ExecutionResult::new_payment_code_error(
                error,
                max_payment_cost,
                account_main_purse_balance,
                session_result.cost(),
                entity_main_purse_key,
                rewards_target_purse_balance_key,
            ) {
                Ok(execution_result) => return Ok(execution_result),
                Err(error) => return Ok(ExecutionResult::precondition_failure(error)),
            }
        }

        let post_session_rc = if session_result.is_failure() {
            // If session code fails we do not include its effects,
            // so we start again from the post-payment state.
            Rc::new(RefCell::new(post_payment_tracking_copy.fork()))
        } else {
            session_result = session_result.with_effects(session_tracking_copy.borrow().effects());
            session_tracking_copy
        };

        // NOTE: session_code_spec_3: (do not include session execution effects in
        // results) is enforced in execution_result_builder.build()
        execution_result_builder.set_session_execution_result(session_result);

        // payment_code_spec_5: run finalize process
        let finalize_result: ExecutionResult = {
            let post_session_tc = post_session_rc.borrow();
            let finalization_tc = Rc::new(RefCell::new(post_session_tc.fork()));

            let handle_payment_args = {
                //((gas spent during payment code execution) + (gas spent during session code execution)) * gas_price
                let finalize_cost_motes = match Motes::from_gas(
                    execution_result_builder.total_cost(),
                    deploy_item.gas_price,
                ) {
                    Some(motes) => motes,
                    None => {
                        return Ok(ExecutionResult::precondition_failure(
                            Error::GasConversionOverflow,
                        ))
                    }
                };

                let maybe_runtime_args = RuntimeArgs::try_new(|args| {
                    args.insert(handle_payment::ARG_AMOUNT, finalize_cost_motes.value())?;
                    args.insert(handle_payment::ARG_ACCOUNT, account_hash)?;
                    args.insert(handle_payment::ARG_TARGET, rewards_target_purse)?;
                    Ok(())
                });
                match maybe_runtime_args {
                    Ok(runtime_args) => runtime_args,
                    Err(error) => {
                        let exec_error = ExecError::from(error);
                        return Ok(ExecutionResult::precondition_failure(exec_error.into()));
                    }
                }
            };

            // The Handle Payment keys may have changed because of effects during payment and/or
            // session, so we need to look them up again from the tracking copy
            let system_contract_registry = finalization_tc.borrow_mut().get_system_contracts()?;

            let handle_payment_contract_hash = system_contract_registry
                .get(HANDLE_PAYMENT)
                .ok_or_else(|| {
                    error!("Missing system handle payment contract hash");
                    Error::MissingSystemContractHash(HANDLE_PAYMENT.to_string())
                })?;

            let handle_payment_contract = match finalization_tc
                .borrow_mut()
                .get_addressable_entity(*handle_payment_contract_hash)
            {
                Ok(info) => info,
                Err(error) => return Ok(ExecutionResult::precondition_failure(error.into())),
            };

            let handle_payment_addr =
                EntityAddr::new_system_entity_addr(handle_payment_contract_hash.value());

            let handle_payment_named_keys = match finalization_tc
                .borrow_mut()
                .get_named_keys(handle_payment_addr)
            {
                Ok(named_keys) => named_keys,
                Err(error) => return Ok(ExecutionResult::precondition_failure(error.into())),
            };

            let mut handle_payment_access_rights = handle_payment_contract
                .extract_access_rights(*handle_payment_contract_hash, &handle_payment_named_keys);
            handle_payment_access_rights.extend(&[payment_purse_uref, rewards_target_purse]);

            let gas_limit = Gas::new(U512::MAX);

            let handle_payment_stack = self.get_new_system_call_stack();
            let system_account_hash = PublicKey::System.to_account_hash();

            let (_ret, finalize_result): (Option<()>, ExecutionResult) = executor
                .call_system_contract(
                    DirectSystemContractCall::FinalizePayment,
                    handle_payment_args,
                    &system_addressable_entity,
                    EntityKind::Account(system_account_hash),
                    authorization_keys,
                    system_account_hash,
                    blocktime,
                    deploy_hash,
                    gas_limit,
                    protocol_version,
                    finalization_tc,
                    Phase::FinalizePayment,
                    handle_payment_stack,
                    U512::zero(),
                );

            finalize_result
        };

        execution_result_builder.set_finalize_execution_result(finalize_result);

        // We panic here to indicate that the builder was not used properly.
        let ret = execution_result_builder
            .build()
            .expect("ExecutionResultBuilder not initialized properly");

        // NOTE: payment_code_spec_5_a is enforced in execution_result_builder.build()
        // payment_code_spec_6: return properly combined set of transforms and
        // appropriate error
        Ok(ret)
    }

    fn get_rewards_purse(
        &self,
        protocol_version: ProtocolVersion,
        proposer: PublicKey,
        prestate_hash: Digest,
    ) -> Result<URef, Error> {
        let tracking_copy = match self.tracking_copy(prestate_hash) {
            Err(tce) => return Err(Error::TrackingCopy(tce)),
            Ok(None) => return Err(Error::RootNotFound(prestate_hash)),
            Ok(Some(tracking_copy)) => Rc::new(RefCell::new(tracking_copy)),
        };
        match self.config.fee_handling() {
            FeeHandling::PayToProposer => {
                // the proposer of the block this deploy is in receives the gas from this deploy
                // execution
                let proposer_account: AddressableEntity = match tracking_copy
                    .borrow_mut()
                    .get_addressable_entity_by_account_hash(
                        protocol_version,
                        AccountHash::from(&proposer),
                    ) {
                    Ok(account) => account,
                    Err(error) => return Err(error.into()),
                };

                Ok(proposer_account.main_purse())
            }
            FeeHandling::Accumulate => {
                let handle_payment_hash = self.get_handle_payment_hash(prestate_hash)?;

                let handle_payment_named_keys = tracking_copy
                    .borrow_mut()
                    .get_named_keys(EntityAddr::System(handle_payment_hash.value()))?;

                let accumulation_purse_uref =
                    match handle_payment_named_keys.get(ACCUMULATION_PURSE_KEY) {
                        Some(Key::URef(accumulation_purse)) => accumulation_purse,
                        Some(_) | None => {
                            error!(
                            "fee handling is configured to accumulate but handle payment does not \
                            have accumulation purse"
                        );
                            return Err(Error::FailedToRetrieveAccumulationPurse);
                        }
                    };

                Ok(*accumulation_purse_uref)
            }
            FeeHandling::Burn => Ok(URef::default()),
        }
    }

    /// Commit effects of the execution.
    ///
    /// This method has to be run after an execution has been made to persists the effects of it.
    ///
    /// Returns new state root hash.
    pub fn commit_effects(
        &self,
        pre_state_hash: Digest,
        effects: Effects,
    ) -> Result<Digest, Error> {
        self.state
            .commit(pre_state_hash, effects)
            .map_err(|err| Error::Exec(err.into()))
    }

    /// Gets a trie object for given state root hash.
    pub fn get_trie_full(&self, trie_key: Digest) -> Result<Option<TrieRaw>, Error> {
        match self.state.get_trie_full(&trie_key) {
            Ok(ret) => Ok(ret),
            Err(err) => Err(err.into()),
        }
    }

    /// Puts a trie if no children are missing from the global state; otherwise reports the missing
    /// children hashes via the `Error` enum.
    pub fn put_trie_if_all_children_present(&self, trie_bytes: &[u8]) -> Result<Digest, Error> {
        let missing_children = match self.state.missing_children(trie_bytes) {
            Ok(ret) => ret,
            Err(err) => return Err(err.into()),
        };
        if missing_children.is_empty() {
            Ok(self.state.put_trie(trie_bytes)?)
        } else {
            Err(Error::MissingTrieNodeChildren(missing_children))
        }
    }

    /// Obtains validator weights for given era.
    ///
    /// This skips execution of auction's `get_era_validator` entry point logic to avoid creating an
    /// executor instance, and going through the execution flow. It follows the same process but
    /// uses queries rather than execution to get the snapshot.
    pub fn get_era_validators(
        &self,
        get_era_validators_request: EraValidatorsRequest,
    ) -> EraValidatorsResult {
        let state_root_hash = get_era_validators_request.state_hash();

        let system_contract_registry = match self.get_system_contract_registry(state_root_hash) {
            Ok(system_contract_registry) => system_contract_registry,
            Err(error) => {
                error!(%state_root_hash, %error, "auction not found");
                return EraValidatorsResult::AuctionNotFound;
            }
        };

        let query_request = match system_contract_registry.get(AUCTION).copied() {
            Some(auction_hash) => QueryRequest::new(
                state_root_hash,
                Key::addressable_entity_key(EntityKindTag::System, auction_hash),
                vec![SEIGNIORAGE_RECIPIENTS_SNAPSHOT_KEY.to_string()],
            ),
            None => return EraValidatorsResult::AuctionNotFound,
        };

        let snapshot = match self.run_query(query_request) {
            QueryResult::RootNotFound => return EraValidatorsResult::RootNotFound,
            QueryResult::Failure(error) => {
                error!(%error, "unexpected tracking copy error");
                return EraValidatorsResult::Failure(error);
            }
            QueryResult::ValueNotFound(message) => {
                error!(%message, "value not found");
                return EraValidatorsResult::ValueNotFound(message);
            }
            QueryResult::Success { value, proofs: _ } => {
                let cl_value = match value.into_cl_value() {
                    Some(snapshot_cl_value) => snapshot_cl_value,
                    None => {
                        error!("unexpected query failure; seigniorage recipients snapshot is not a CLValue");
                        return EraValidatorsResult::Failure(
                            TrackingCopyError::UnexpectedStoredValueVariant,
                        );
                    }
                };

                match cl_value.into_t() {
                    Ok(snapshot) => snapshot,
                    Err(cve) => {
                        return EraValidatorsResult::Failure(TrackingCopyError::CLValue(cve));
                    }
                }
            }
        };
        let era_validators = auction::detail::era_validators_from_snapshot(snapshot);
        EraValidatorsResult::Success { era_validators }
    }

    /// Gets current bids from the auction system.
    pub fn get_bids(&self, get_bids_request: BidsRequest) -> BidsResult {
        let state_root_hash = get_bids_request.state_hash();
        let tracking_copy = match self.state.checkout(state_root_hash) {
            Ok(ret) => match ret {
                Some(tracking_copy) => Rc::new(RefCell::new(TrackingCopy::new(
                    tracking_copy,
                    self.config.max_query_depth,
                ))),
                None => return BidsResult::RootNotFound,
            },
            Err(err) => return BidsResult::Failure(TrackingCopyError::Storage(err)),
        };

        let mut tc = tracking_copy.borrow_mut();

        let bid_keys = match tc.get_keys(&KeyTag::BidAddr) {
            Ok(ret) => ret,
            Err(err) => return BidsResult::Failure(err),
        };

        let mut bids = vec![];
        for key in bid_keys.iter() {
            match tc.get(key) {
                Ok(ret) => match ret {
                    Some(StoredValue::BidKind(bid_kind)) => {
                        bids.push(bid_kind);
                    }
                    Some(_) => {
                        return BidsResult::Failure(TrackingCopyError::UnexpectedStoredValueVariant)
                    }
                    None => return BidsResult::Failure(TrackingCopyError::MissingBid(*key)),
                },
                Err(error) => return BidsResult::Failure(error),
            }
        }
        BidsResult::Success { bids }
    }

    /// Distribute block rewards.
    pub fn distribute_block_rewards(
        &self,
        pre_state_hash: Digest,
        protocol_version: ProtocolVersion,
        rewards: &BTreeMap<PublicKey, U512>,
        next_block_height: u64,
        time: u64,
    ) -> Result<Digest, StepError> {
        let tracking_copy = match self.tracking_copy(pre_state_hash) {
            Ok(Some(tracking_copy)) => Rc::new(RefCell::new(tracking_copy)),
            Ok(None) => return Err(StepError::RootNotFound(pre_state_hash)),
            Err(error) => return Err(StepError::OtherEngineStateError(error.into())),
        };

        let executor = Executor::new(self.config().clone());

        let virtual_system_contract_by_account = {
            let system_account_addr = PublicKey::System.to_account_hash();

            tracking_copy
                .borrow_mut()
                .get_addressable_entity_by_account_hash(protocol_version, system_account_addr)
                .map_err(|err| StepError::OtherEngineStateError(Error::TrackingCopy(err)))?
        };

        let authorization_keys = {
            let mut ret = BTreeSet::new();
            ret.insert(PublicKey::System.to_account_hash());
            ret
        };

        let gas_limit = Gas::new(U512::from(std::u64::MAX));

        let deploy_hash = {
            // seeds address generator w/ era_end_timestamp_millis
            let mut bytes = time.into_bytes()?;
            bytes.append(&mut next_block_height.into_bytes()?);
            DeployHash::new(Digest::hash(&bytes))
        };

        let system_account_hash = PublicKey::System.to_account_hash();

        {
            let distribute_accumulated_fees_stack = self.get_new_system_call_stack();
            let (_, execution_result): (Option<()>, ExecutionResult) = executor
                .call_system_contract(
                    DirectSystemContractCall::DistributeAccumulatedFees,
                    RuntimeArgs::default(),
                    &virtual_system_contract_by_account,
                    EntityKind::Account(system_account_hash),
                    authorization_keys.clone(),
                    system_account_hash,
                    BlockTime::default(),
                    deploy_hash,
                    gas_limit,
                    protocol_version,
                    Rc::clone(&tracking_copy),
                    Phase::Session,
                    distribute_accumulated_fees_stack,
                    // There should be no tokens transferred during rewards distribution.
                    U512::zero(),
                );

            if let Some(exec_error) = execution_result.take_error() {
                return Err(StepError::DistributeAccumulatedFeesError(exec_error));
            }
        }

        {
            let mut runtime_args = RuntimeArgs::new();
            runtime_args.insert(ARG_REWARDS_MAP, rewards)?;
            let distribute_rewards_stack = self.get_new_system_call_stack();

            let (_, execution_result): (Option<()>, ExecutionResult) = executor
                .call_system_contract(
                    DirectSystemContractCall::DistributeRewards,
                    runtime_args,
                    &virtual_system_contract_by_account,
                    EntityKind::Account(system_account_hash),
                    authorization_keys,
                    system_account_hash,
                    BlockTime::default(),
                    deploy_hash,
                    gas_limit,
                    protocol_version,
                    Rc::clone(&tracking_copy),
                    Phase::Session,
                    distribute_rewards_stack,
                    // There should be no tokens transferred during rewards distribution.
                    U512::zero(),
                );

            if let Some(exec_error) = execution_result.take_error() {
                return Err(StepError::DistributeError(exec_error));
            }
        }

        let effects = tracking_copy.borrow().effects();

        // commit
        let post_state_hash = self
            .state
            .commit(pre_state_hash, effects)
            .map_err(Into::<Error>::into)?;

        Ok(post_state_hash)
    }

    /// Executes a step request.
    pub fn commit_step(&self, step_request: StepRequest) -> Result<StepSuccess, StepError> {
        let tracking_copy = match self.tracking_copy(step_request.pre_state_hash) {
            Ok(Some(tracking_copy)) => Rc::new(RefCell::new(tracking_copy)),
            Ok(None) => return Err(StepError::RootNotFound(step_request.pre_state_hash)),
            Err(error) => return Err(StepError::OtherEngineStateError(error.into())),
        };

        let executor = Executor::new(self.config().clone());

        let system_account_addr = PublicKey::System.to_account_hash();

        let protocol_version = step_request.protocol_version;

        let system_addressable_entity = tracking_copy
            .borrow_mut()
            .get_addressable_entity_by_account_hash(protocol_version, system_account_addr)
            .map_err(|err| StepError::OtherEngineStateError(Error::TrackingCopy(err)))?;

        let authorization_keys = {
            let mut ret = BTreeSet::new();
            ret.insert(system_account_addr);
            ret
        };

        let gas_limit = Gas::new(U512::from(std::u64::MAX));
        let deploy_hash = {
            // seeds address generator w/ era_end_timestamp_millis
            let mut bytes = step_request.era_end_timestamp_millis.into_bytes()?;
            bytes.append(&mut step_request.next_era_id.into_bytes()?);
            DeployHash::new(Digest::hash(&bytes))
        };

        let slashed_validators: Vec<PublicKey> = step_request.slashed_validators();

        if !slashed_validators.is_empty() {
            let slash_args = {
                let mut runtime_args = RuntimeArgs::new();
                runtime_args
                    .insert(ARG_VALIDATOR_PUBLIC_KEYS, slashed_validators)
                    .map_err(|e| Error::Exec(e.into()))?;
                runtime_args
            };

            let slash_stack = self.get_new_system_call_stack();
            let system_account_hash = PublicKey::System.to_account_hash();
            let (_, execution_result): (Option<()>, ExecutionResult) = executor
                .call_system_contract(
                    DirectSystemContractCall::Slash,
                    slash_args,
                    &system_addressable_entity,
                    EntityKind::Account(system_account_hash),
                    authorization_keys.clone(),
                    system_account_hash,
                    BlockTime::default(),
                    deploy_hash,
                    gas_limit,
                    step_request.protocol_version,
                    Rc::clone(&tracking_copy),
                    Phase::Session,
                    slash_stack,
                    // No transfer should occur when slashing.
                    U512::zero(),
                );

            if let Some(exec_error) = execution_result.take_error() {
                return Err(StepError::SlashingError(exec_error));
            }
        }

        let run_auction_args = RuntimeArgs::try_new(|args| {
            args.insert(
                ARG_ERA_END_TIMESTAMP_MILLIS,
                step_request.era_end_timestamp_millis,
            )?;
            args.insert(
                ARG_EVICTED_VALIDATORS,
                step_request
                    .evict_items
                    .iter()
                    .map(|item| item.validator_id.clone())
                    .collect::<Vec<PublicKey>>(),
            )?;
            Ok(())
        })?;

        let run_auction_stack = self.get_new_system_call_stack();
        let system_account_hash = PublicKey::System.to_account_hash();
        let (_, execution_result): (Option<()>, ExecutionResult) = executor.call_system_contract(
            DirectSystemContractCall::RunAuction,
            run_auction_args,
            &system_addressable_entity,
            EntityKind::Account(system_account_hash),
            authorization_keys,
            system_account_hash,
            BlockTime::default(),
            deploy_hash,
            gas_limit,
            step_request.protocol_version,
            Rc::clone(&tracking_copy),
            Phase::Session,
            run_auction_stack,
            // RunAuction should not consume tokens.
            U512::zero(),
        );

        if let Some(exec_error) = execution_result.take_error() {
            return Err(StepError::AuctionError(exec_error));
        }

        let effects = tracking_copy.borrow().effects();

        // commit
        let post_state_hash = self
            .state
            .commit(step_request.pre_state_hash, effects.clone())
            .map_err(Into::<Error>::into)?;

        Ok(StepSuccess {
            post_state_hash,
            effects,
        })
    }

    /// Gets the balance of a given public key.
    pub fn get_balance(
        &self,
        state_hash: Digest,
        public_key: PublicKey,
    ) -> Result<BalanceResult, Error> {
        // Look up the account, get the main purse, and then do the existing balance check
        let tracking_copy = match self.tracking_copy(state_hash) {
            Ok(Some(tracking_copy)) => Rc::new(RefCell::new(tracking_copy)),
            Ok(None) => return Ok(BalanceResult::RootNotFound),
            Err(error) => return Err(error.into()),
        };

        let account_addr = public_key.to_account_hash();

        let entity_hash = match tracking_copy
            .borrow_mut()
            .get_entity_hash_by_account_hash(account_addr)
        {
            Ok(account) => account,
            Err(error) => return Err(error.into()),
        };

        let account = match tracking_copy
            .borrow_mut()
            .read(&Key::addressable_entity_key(
                EntityKindTag::Account,
                entity_hash,
            ))
            .map_err(|_| Error::InvalidKeyVariant)?
        {
            Some(StoredValue::AddressableEntity(account)) => account,
            Some(_) | None => return Err(Error::InvalidKeyVariant),
        };

        let main_purse_balance_key = {
            let main_purse = account.main_purse();
            match tracking_copy
                .borrow()
                .get_purse_balance_key(main_purse.into())
            {
                Ok(balance_key) => balance_key,
                Err(error) => return Err(error.into()),
            }
        };

        let (account_balance, proof) = match tracking_copy
            .borrow()
            .get_purse_balance_with_proof(main_purse_balance_key)
        {
            Ok((balance, proof)) => (balance, proof),
            Err(error) => return Err(error.into()),
        };

        let proof = Box::new(proof);
        let motes = account_balance.value();
        Ok(BalanceResult::Success { motes, proof })
    }

    /// Obtains an instance of a system contract registry for a given state root hash.
    pub fn get_system_contract_registry(
        &self,
        state_root_hash: Digest,
    ) -> Result<SystemContractRegistry, Error> {
        let tracking_copy = match self.tracking_copy(state_root_hash)? {
            None => return Err(Error::RootNotFound(state_root_hash)),
            Some(tracking_copy) => Rc::new(RefCell::new(tracking_copy)),
        };
        let result = tracking_copy
            .borrow_mut()
            .get_system_contracts()
            .map_err(|error| {
                warn!(%error, "Failed to retrieve system contract registry");
                Error::MissingSystemContractRegistry
            });
        result
    }

    /// Returns mint system contract hash.
    pub fn get_system_mint_hash(&self, state_hash: Digest) -> Result<AddressableEntityHash, Error> {
        let registry = self.get_system_contract_registry(state_hash)?;
        let mint_hash = registry.get(MINT).ok_or_else(|| {
            error!("Missing system mint contract hash");
            Error::MissingSystemContractHash(MINT.to_string())
        })?;
        Ok(*mint_hash)
    }

    /// Returns auction system contract hash.
    pub fn get_system_auction_hash(
        &self,
        state_hash: Digest,
    ) -> Result<AddressableEntityHash, Error> {
        let registry = self.get_system_contract_registry(state_hash)?;
        let auction_hash = registry.get(AUCTION).ok_or_else(|| {
            error!("Missing system auction contract hash");
            Error::MissingSystemContractHash(AUCTION.to_string())
        })?;
        Ok(*auction_hash)
    }

    /// Returns handle payment system contract hash.
    pub fn get_handle_payment_hash(
        &self,
        state_hash: Digest,
    ) -> Result<AddressableEntityHash, Error> {
        let registry = self.get_system_contract_registry(state_hash)?;
        let handle_payment = registry.get(HANDLE_PAYMENT).ok_or_else(|| {
            error!("Missing system handle payment contract hash");
            Error::MissingSystemContractHash(HANDLE_PAYMENT.to_string())
        })?;
        Ok(*handle_payment)
    }

    fn get_new_system_call_stack(&self) -> RuntimeStack {
        let max_height = self.config.max_runtime_call_stack_height() as usize;
        RuntimeStack::new_system_call_stack(max_height)
    }

    /// Returns the checksum registry at the given state root hash.
    pub fn get_checksum_registry(
        &self,
        state_root_hash: Digest,
    ) -> Result<Option<ChecksumRegistry>, Error> {
        let tracking_copy = match self.tracking_copy(state_root_hash)? {
            None => return Err(Error::RootNotFound(state_root_hash)),
            Some(tracking_copy) => Rc::new(RefCell::new(tracking_copy)),
        };
        let maybe_checksum_registry = tracking_copy
            .borrow_mut()
            .get_checksum_registry()
            .map_err(Error::TrackingCopy);
        maybe_checksum_registry
    }

    /// Returns the Merkle proof for the checksum registry at the given state root hash.
    pub fn get_checksum_registry_proof(
        &self,
        state_root_hash: Digest,
    ) -> Result<TrieMerkleProof<Key, StoredValue>, Error> {
        let tracking_copy = match self.tracking_copy(state_root_hash)? {
            None => return Err(Error::RootNotFound(state_root_hash)),
            Some(tracking_copy) => Rc::new(RefCell::new(tracking_copy)),
        };

        let key = Key::ChecksumRegistry;
        let maybe_proof = tracking_copy.borrow_mut().reader().read_with_proof(&key)?;
        maybe_proof.ok_or(Error::MissingChecksumRegistry)
    }
}

fn log_execution_result(preamble: &'static str, result: &ExecutionResult) {
    trace!("{}: {:?}", preamble, result);
    match result {
        ExecutionResult::Success {
            transfers,
            cost,
            effects,
            messages,
        } => {
            debug!(
                %cost,
                transfer_count = %transfers.len(),
                transforms_count = %effects.len(),
                messages_count = %messages.len(),
                "{}: execution success",
                preamble
            );
        }
        ExecutionResult::Failure {
            error,
            transfers,
            cost,
            effects,
            messages,
        } => {
            debug!(
                %error,
                %cost,
                transfer_count = %transfers.len(),
                transforms_count = %effects.len(),
                messages_count = %messages.len(),
                "{}: execution failure",
                preamble
            );
        }
    }
}

fn should_charge_for_errors_in_wasm(execution_result: &ExecutionResult) -> bool {
    match execution_result {
        ExecutionResult::Failure {
            error,
            transfers: _,
            cost: _,
            effects: _,
            messages: _,
        } => match error {
            Error::Exec(err) => match err {
                ExecError::WasmPreprocessing(_) | ExecError::UnsupportedWasmStart => true,
                ExecError::Storage(_)
                | ExecError::InvalidByteCode(_)
                | ExecError::WasmOptimizer
                | ExecError::ParityWasm(_)
                | ExecError::Interpreter(_)
                | ExecError::BytesRepr(_)
                | ExecError::NamedKeyNotFound(_)
                | ExecError::KeyNotFound(_)
                | ExecError::AccountNotFound(_)
                | ExecError::TypeMismatch(_)
                | ExecError::InvalidAccess { .. }
                | ExecError::ForgedReference(_)
                | ExecError::URefNotFound(_)
                | ExecError::FunctionNotFound(_)
                | ExecError::GasLimit
                | ExecError::Ret(_)
                | ExecError::Resolver(_)
                | ExecError::Revert(_)
                | ExecError::AddKeyFailure(_)
                | ExecError::RemoveKeyFailure(_)
                | ExecError::UpdateKeyFailure(_)
                | ExecError::SetThresholdFailure(_)
                | ExecError::SystemContract(_)
                | ExecError::DeploymentAuthorizationFailure
                | ExecError::UpgradeAuthorizationFailure
                | ExecError::ExpectedReturnValue
                | ExecError::UnexpectedReturnValue
                | ExecError::InvalidContext
                | ExecError::IncompatibleProtocolMajorVersion { .. }
                | ExecError::CLValue(_)
                | ExecError::HostBufferEmpty
                | ExecError::NoActiveEntityVersions(_)
                | ExecError::InvalidEntityVersion(_)
                | ExecError::NoSuchMethod(_)
                | ExecError::TemplateMethod(_)
                | ExecError::KeyIsNotAURef(_)
                | ExecError::UnexpectedStoredValueVariant
                | ExecError::LockedEntity(_)
                | ExecError::InvalidPackage(_)
                | ExecError::InvalidEntity(_)
                | ExecError::MissingArgument { .. }
                | ExecError::DictionaryItemKeyExceedsLength
                | ExecError::MissingSystemContractRegistry
                | ExecError::MissingSystemContractHash(_)
                | ExecError::RuntimeStackOverflow
                | ExecError::ValueTooLarge
                | ExecError::MissingRuntimeStack
                | ExecError::DisabledEntity(_)
                | ExecError::UnexpectedKeyVariant(_)
                | ExecError::InvalidEntityKind(_)
                | ExecError::TrackingCopy(_)
                | ExecError::Transform(_)
                | ExecError::InvalidEntryPointType
                | ExecError::InvalidMessageTopicOperation
                | ExecError::InvalidUtf8Encoding(_) => false,
                ExecError::DisabledUnrestrictedTransfers => false,
            },
            Error::WasmPreprocessing(_) => true,
            Error::WasmSerialization(_) => true,
            Error::RootNotFound(_)
            | Error::InvalidProtocolVersion(_)
            | Error::Genesis(_)
            | Error::Storage(_)
            | Error::Authorization
            | Error::MissingContractByAccountHash(_)
            | Error::MissingEntityPackage(_)
            | Error::InsufficientPayment
            | Error::GasConversionOverflow
            | Error::Deploy
            | Error::Finalization
            | Error::Bytesrepr(_)
            | Error::Mint(_)
            | Error::InvalidKeyVariant
            | Error::ProtocolUpgrade(_)
            | Error::InvalidDeployItemVariant(_)
            | Error::CommitError(_)
            | Error::MissingSystemContractRegistry
            | Error::MissingSystemContractHash(_)
            | Error::MissingChecksumRegistry
            | Error::RuntimeStackOverflow
            | Error::FailedToGetKeys(_)
            | Error::FailedToGetStoredWithdraws
            | Error::FailedToGetWithdrawPurses
            | Error::FailedToRetrieveUnbondingDelay
            | Error::FailedToRetrieveEraId
            | Error::MissingTrieNodeChildren(_)
            | Error::FailedToRetrieveAccumulationPurse
            | Error::FailedToPrune(_)
            | Error::TrackingCopy(_) => false,
        },
        ExecutionResult::Success { .. } => false,
    }
}