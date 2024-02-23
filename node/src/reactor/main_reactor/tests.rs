use std::{
    collections::{BTreeMap, HashSet},
    fs, iter,
    net::SocketAddr,
    str::FromStr,
    sync::Arc,
    time::Duration,
};

use either::Either;
use num::Zero;
use num_rational::Ratio;
use rand::Rng;
use tempfile::TempDir;
use tokio::time::{self, error::Elapsed};
use tracing::{error, info};

use casper_execution_engine::core::engine_state::GetBidsRequest;
use casper_types::{
    system::auction::{Bids, DelegationRate},
    testing::TestRng,
    EraId, Motes, ProtocolVersion, PublicKey, SecretKey, TimeDiff, Timestamp, U512,
};

use crate::{
    components::{
        consensus::{
            self, ClContext, ConsensusMessage, HighwayMessage, HighwayVertex, NewBlockPayload,
        },
        gossiper, network, storage,
        upgrade_watcher::NextUpgrade,
        ComponentState,
    },
    effect::{incoming::ConsensusMessageIncoming, requests::ContractRuntimeRequest, EffectExt},
    protocol::Message,
    reactor::{
        main_reactor::{Config, MainEvent, MainReactor, ReactorState},
        Runner,
    },
    testing::{
        self, filter_reactor::FilterReactor, network::TestingNetwork, ConditionCheckReactor,
    },
    types::{
        chainspec::{AccountConfig, AccountsConfig, ValidatorConfig},
        ActivationPoint, AvailableBlockRange, Block, BlockHash, BlockHeader, BlockPayload,
        Chainspec, ChainspecRawBytes, Deploy, ExitCode, NodeId, SyncHandling,
    },
    utils::{extract_metric_names, External, Fuse, Loadable, Source, RESOURCES_PATH},
    WithDir,
};

const ERA_ZERO: EraId = EraId::new(0);
const ERA_ONE: EraId = EraId::new(1);
const ERA_TWO: EraId = EraId::new(2);
const ERA_THREE: EraId = EraId::new(3);
const TEN_SECS: Duration = Duration::from_secs(10);
const ONE_MIN: Duration = Duration::from_secs(60);

type Nodes = testing::network::Nodes<FilterReactor<MainReactor>>;

impl Runner<ConditionCheckReactor<FilterReactor<MainReactor>>> {
    fn main_reactor(&self) -> &MainReactor {
        self.reactor().inner().inner()
    }
}

enum InitialStakes {
    FromVec(Vec<u128>),
    Random { count: usize },
    AllEqual { count: usize, stake: u128 },
}

struct ChainspecOverride {
    minimum_block_time: TimeDiff,
    minimum_era_height: u64,
}

impl Default for ChainspecOverride {
    fn default() -> Self {
        ChainspecOverride {
            minimum_block_time: "1second".parse().unwrap(),
            minimum_era_height: 2,
        }
    }
}

struct NodeContext {
    id: NodeId,
    secret_key: Arc<SecretKey>,
    config: Config,
    storage_dir: TempDir,
}

struct TestFixture {
    rng: TestRng,
    node_contexts: Vec<NodeContext>,
    network: TestingNetwork<FilterReactor<MainReactor>>,
    chainspec: Arc<Chainspec>,
    chainspec_raw_bytes: Arc<ChainspecRawBytes>,
}

impl TestFixture {
    /// Sets up a new fixture with the number of nodes indicated by `initial_stakes`.
    ///
    /// If `one_second_blocks` is true, then `chainspec.core_config.minimum_block_time` is set to
    /// one sec, otherwise it as per the chainspec in resources/local/chainspec.toml.
    ///
    /// Runs the network until all nodes are initialized (i.e. none of their reactor states are
    /// still `ReactorState::Initialize`).
    async fn new(initial_stakes: InitialStakes, spec_override: Option<ChainspecOverride>) -> Self {
        let mut rng = TestRng::new();
        let stake_values = match initial_stakes {
            InitialStakes::FromVec(stakes) => {
                stakes.into_iter().map(|stake| stake.into()).collect()
            }
            InitialStakes::Random { count } => {
                // By default we use very large stakes so we would catch overflow issues.
                iter::from_fn(|| Some(U512::from(rng.gen_range(100..999)) * U512::from(u128::MAX)))
                    .take(count)
                    .collect()
            }
            InitialStakes::AllEqual { count, stake } => {
                vec![stake.into(); count]
            }
        };

        let secret_keys: Vec<Arc<SecretKey>> = (0..stake_values.len())
            .map(|_| Arc::new(SecretKey::random(&mut rng)))
            .collect();

        let stakes = secret_keys
            .iter()
            .zip(stake_values)
            .map(|(secret_key, stake)| (PublicKey::from(secret_key.as_ref()), stake))
            .collect();
        Self::new_with_keys(rng, secret_keys, stakes, spec_override).await
    }

    async fn new_with_keys(
        mut rng: TestRng,
        secret_keys: Vec<Arc<SecretKey>>,
        stakes: BTreeMap<PublicKey, U512>,
        spec_override: Option<ChainspecOverride>,
    ) -> Self {
        testing::init_logging();

        // Load the `local` chainspec.
        let (mut chainspec, chainspec_raw_bytes) =
            <(Chainspec, ChainspecRawBytes)>::from_resources("local");

        // Override accounts with those generated from the keys.
        let accounts = stakes
            .into_iter()
            .map(|(public_key, bonded_amount)| {
                let validator_config =
                    ValidatorConfig::new(Motes::new(bonded_amount), DelegationRate::zero());
                AccountConfig::new(
                    public_key,
                    Motes::new(U512::from(rng.gen_range(10000..99999999))),
                    Some(validator_config),
                )
            })
            .collect();
        let delegators = vec![];
        let administrators = vec![];
        chainspec.network_config.accounts_config =
            AccountsConfig::new(accounts, delegators, administrators);

        // Allow 2 seconds startup time per validator.
        let genesis_time = Timestamp::now() + TimeDiff::from_seconds(secret_keys.len() as u32 * 2);
        info!(
            "creating test chain configuration, genesis: {}",
            genesis_time
        );
        chainspec.protocol_config.activation_point = ActivationPoint::Genesis(genesis_time);
        chainspec.core_config.finality_threshold_fraction = Ratio::new(34, 100);
        chainspec.core_config.era_duration = TimeDiff::from_millis(0);
        chainspec.core_config.auction_delay = 1;
        chainspec.core_config.unbonding_delay = 3;
        chainspec.core_config.validator_slots = 100;
        let spec_override = spec_override.unwrap_or_default();
        chainspec.core_config.minimum_block_time = spec_override.minimum_block_time;
        chainspec.core_config.minimum_era_height = spec_override.minimum_era_height;
        chainspec.highway_config.maximum_round_length =
            chainspec.core_config.minimum_block_time * 2;

        let mut fixture = TestFixture {
            rng,
            node_contexts: vec![],
            network: TestingNetwork::new(),
            chainspec: Arc::new(chainspec),
            chainspec_raw_bytes: Arc::new(chainspec_raw_bytes),
        };

        for secret_key in secret_keys {
            let (config, storage_dir) = fixture.create_node_config(secret_key.as_ref(), None);
            fixture.add_node(secret_key, config, storage_dir).await;
        }

        fixture
            .run_until(
                move |nodes: &Nodes| {
                    nodes.values().all(|runner| {
                        !matches!(runner.main_reactor().state, ReactorState::Initialize)
                    })
                },
                Duration::from_secs(20),
            )
            .await;

        fixture
    }

    /// Access the environments RNG.
    #[inline(always)]
    pub fn rng_mut(&mut self) -> &mut TestRng {
        &mut self.rng
    }

    /// Returns the highest complete block from node 0.
    ///
    /// Panics if there is no such block.
    fn highest_complete_block(&self) -> Block {
        let node_0 = self
            .node_contexts
            .first()
            .expect("should have at least one node")
            .id;
        self.network
            .nodes()
            .get(&node_0)
            .expect("should have node 0")
            .main_reactor()
            .storage()
            .read_highest_complete_block()
            .expect("should not error reading db")
            .expect("node 0 should have a complete block")
    }

    fn create_node_config(
        &mut self,
        secret_key: &SecretKey,
        maybe_trusted_hash: Option<BlockHash>,
    ) -> (Config, TempDir) {
        // Set the network configuration.
        let network_cfg = match self.node_contexts.first() {
            Some(first_node) => {
                let known_address =
                    SocketAddr::from_str(&first_node.config.network.bind_address).unwrap();
                network::Config::default_local_net(known_address.port())
            }
            None => {
                let port = testing::unused_port_on_localhost();
                network::Config::default_local_net_first_node(port)
            }
        };
        let mut cfg = Config {
            network: network_cfg,
            gossip: gossiper::Config::new_with_small_timeouts(),
            ..Default::default()
        };

        // Additionally set up storage in a temporary directory.
        let (storage_cfg, temp_dir) = storage::Config::default_for_tests();
        // ...and the secret key for our validator.
        {
            let secret_key_path = temp_dir.path().join("secret_key");
            secret_key
                .to_file(secret_key_path.clone())
                .expect("could not write secret key");
            cfg.consensus.secret_key_path = External::Path(secret_key_path);
        }
        cfg.storage = storage_cfg;
        cfg.node.trusted_hash = maybe_trusted_hash;

        (cfg, temp_dir)
    }

    /// Adds a node to the network.
    ///
    /// If a previously-removed node is to be re-added, then the `secret_key`, `config` and
    /// `storage_dir` returned in the `NodeContext` during removal should be used here in order to
    /// ensure the same storage dir is used across both executions.
    async fn add_node(
        &mut self,
        secret_key: Arc<SecretKey>,
        config: Config,
        storage_dir: TempDir,
    ) -> NodeId {
        let (id, _) = self
            .network
            .add_node_with_config_and_chainspec(
                WithDir::new(RESOURCES_PATH.join("local"), config.clone()),
                Arc::clone(&self.chainspec),
                Arc::clone(&self.chainspec_raw_bytes),
                &mut self.rng,
            )
            .await
            .expect("could not add node to reactor");
        let node_context = NodeContext {
            id,
            secret_key,
            config,
            storage_dir,
        };
        self.node_contexts.push(node_context);
        info!("added node {} with id {}", self.node_contexts.len() - 1, id);
        id
    }

    fn remove_and_stop_node(&mut self, index: usize) -> NodeContext {
        let node_context = self.node_contexts.remove(index);
        let runner = self.network.remove_node(&node_context.id).unwrap();
        runner.is_shutting_down.set();
        info!("removed node {} with id {}", index, node_context.id);
        node_context
    }

    /// Runs the network until `condition` is true.
    ///
    /// Returns an error if the condition isn't met in time.
    async fn try_run_until<F>(&mut self, condition: F, within: Duration) -> Result<(), Elapsed>
    where
        F: Fn(&Nodes) -> bool,
    {
        self.network
            .try_settle_on(&mut self.rng, condition, within)
            .await
    }

    /// Runs the network until `condition` is true.
    ///
    /// Panics if the condition isn't met in time.
    async fn run_until<F>(&mut self, condition: F, within: Duration)
    where
        F: Fn(&Nodes) -> bool,
    {
        self.network
            .settle_on(&mut self.rng, condition, within)
            .await
    }

    /// Runs the network until all nodes reach the given completed block height.
    ///
    /// Returns an error if the condition isn't met in time.
    async fn try_run_until_block_height(
        &mut self,
        block_height: u64,
        within: Duration,
    ) -> Result<(), Elapsed> {
        self.try_run_until(
            move |nodes: &Nodes| {
                nodes.values().all(|runner| {
                    runner
                        .main_reactor()
                        .storage()
                        .read_highest_complete_block()
                        .expect("should not error reading db")
                        .map(|block| block.height())
                        == Some(block_height)
                })
            },
            within,
        )
        .await
    }

    /// Runs the network until all nodes reach the given completed block height.
    ///
    /// Panics if the condition isn't met in time.
    async fn run_until_block_height(&mut self, block_height: u64, within: Duration) {
        self.try_run_until_block_height(block_height, within)
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "should reach block {} within {} seconds",
                    block_height,
                    within.as_secs_f64(),
                )
            })
    }

    /// Runs the network until all nodes' consensus components reach the given era.
    ///
    /// Panics if the condition isn't met in time.
    async fn run_until_consensus_in_era(&mut self, era_id: EraId, within: Duration) {
        self.try_run_until(
            move |nodes: &Nodes| {
                nodes
                    .values()
                    .all(|runner| runner.main_reactor().consensus().current_era() == Some(era_id))
            },
            within,
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "should reach {} within {} seconds",
                era_id,
                within.as_secs_f64(),
            )
        })
    }

    /// Runs the network until all nodes' storage components have stored the switch block header for
    /// the given era.
    ///
    /// Panics if the condition isn't met in time.
    async fn run_until_stored_switch_block_header(&mut self, era_id: EraId, within: Duration) {
        self.try_run_until(
            move |nodes: &Nodes| {
                nodes.values().all(|runner| {
                    runner
                        .main_reactor()
                        .storage()
                        .read_highest_switch_block_headers(1)
                        .unwrap()
                        .last()
                        .map_or(false, |header| header.era_id() == era_id)
                })
            },
            within,
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "should have stored switch block header for {} within {} seconds",
                era_id,
                within.as_secs_f64(),
            )
        })
    }

    async fn schedule_upgrade_for_era_two(&mut self) {
        for runner in self.network.runners_mut() {
            runner
                .process_injected_effects(|effect_builder| {
                    let upgrade = NextUpgrade::new(
                        ActivationPoint::EraId(ERA_TWO),
                        ProtocolVersion::from_parts(999, 0, 0),
                    );
                    effect_builder
                        .announce_upgrade_activation_point_read(upgrade)
                        .ignore()
                })
                .await;
        }
    }

    #[inline(always)]
    pub fn network_mut(&mut self) -> &mut TestingNetwork<FilterReactor<MainReactor>> {
        &mut self.network
    }

    pub fn run_until_stopped(
        self,
        rng: TestRng,
    ) -> impl futures::Future<Output = (TestingNetwork<FilterReactor<MainReactor>>, TestRng)> {
        self.network.crank_until_stopped(rng)
    }
}

/// Given a block height and a node id, returns a predicate to check if the lowest available block
/// for the specified node is at or below the specified height.
fn node_has_lowest_available_block_at_or_below_height(
    height: u64,
    node_id: NodeId,
) -> impl Fn(&Nodes) -> bool {
    move |nodes: &Nodes| {
        nodes.get(&node_id).map_or(true, |runner| {
            let available_block_range = runner.main_reactor().storage().get_available_block_range();
            if available_block_range.low() == 0 && available_block_range.high() == 0 {
                false
            } else {
                available_block_range.low() <= height
            }
        })
    }
}

fn is_ping(event: &MainEvent) -> bool {
    if let MainEvent::ConsensusMessageIncoming(ConsensusMessageIncoming { message, .. }) = event {
        if let ConsensusMessage::Protocol { ref payload, .. } = **message {
            return matches!(
                payload.deserialize_incoming::<HighwayMessage::<ClContext>>(),
                Ok(HighwayMessage::<ClContext>::NewVertex(HighwayVertex::Ping(
                    _
                )))
            );
        }
    }
    false
}

/// A set of consecutive switch blocks.
struct SwitchBlocks {
    headers: Vec<BlockHeader>,
}

impl SwitchBlocks {
    /// Collects all switch blocks of the first `era_count` eras, and asserts that they are equal
    /// in all nodes.
    fn collect(nodes: &Nodes, era_count: u64) -> SwitchBlocks {
        let mut headers = Vec::new();
        for era_number in 0..era_count {
            let mut header_iter = nodes.values().map(|runner| {
                let storage = runner.main_reactor().storage();
                let maybe_block = storage
                    .read_switch_block_by_era_id(EraId::from(era_number))
                    .expect("failed to get switch block by era id");
                maybe_block.expect("missing switch block").take_header()
            });
            let header = header_iter.next().unwrap();
            assert_eq!(era_number, header.era_id().value());
            for other_header in header_iter {
                assert_eq!(header, other_header);
            }
            headers.push(header);
        }
        SwitchBlocks { headers }
    }

    /// Returns the list of equivocators in the given era.
    fn equivocators(&self, era_number: u64) -> &[PublicKey] {
        &self.headers[era_number as usize]
            .era_end()
            .expect("era end")
            .era_report()
            .equivocators
    }

    /// Returns the list of inactive validators in the given era.
    fn inactive_validators(&self, era_number: u64) -> &[PublicKey] {
        &self.headers[era_number as usize]
            .era_end()
            .expect("era end")
            .era_report()
            .inactive_validators
    }

    /// Returns the list of validators in the successor era.
    fn next_era_validators(&self, era_number: u64) -> &BTreeMap<PublicKey, U512> {
        self.headers[era_number as usize]
            .next_era_validator_weights()
            .expect("validators")
    }

    /// Returns the set of bids in the auction contract at the end of the given era.
    fn bids(&self, nodes: &Nodes, era_number: u64) -> Bids {
        let correlation_id = Default::default();
        let state_root_hash = *self.headers[era_number as usize].state_root_hash();
        for runner in nodes.values() {
            let request = GetBidsRequest::new(state_root_hash);
            let engine_state = runner.main_reactor().contract_runtime().engine_state();
            let bids_result = engine_state
                .get_bids(correlation_id, request)
                .expect("get_bids failed");
            if let Some(bids) = bids_result.into_success() {
                return bids;
            }
        }
        unreachable!("at least one node should have bids for era {}", era_number);
    }
}

#[tokio::test]
async fn run_network() {
    // Set up a network with five nodes and run until in era 2.
    let initial_stakes = InitialStakes::Random { count: 5 };
    let mut fixture = TestFixture::new(initial_stakes, None).await;
    fixture.run_until_consensus_in_era(ERA_TWO, ONE_MIN).await;
}

#[tokio::test]
async fn historical_sync_with_era_height_1() {
    let initial_stakes = InitialStakes::Random { count: 5 };
    let spec_override = ChainspecOverride {
        minimum_block_time: "4seconds".parse().unwrap(),
        ..Default::default()
    };
    let mut fixture = TestFixture::new(initial_stakes, Some(spec_override)).await;

    // Wait for all nodes to reach era 3.
    fixture.run_until_consensus_in_era(ERA_THREE, ONE_MIN).await;

    // Create a joiner node.
    let secret_key = SecretKey::random(&mut fixture.rng);
    let trusted_hash = *fixture.highest_complete_block().hash();
    let (mut config, storage_dir) = fixture.create_node_config(&secret_key, Some(trusted_hash));
    config.node.sync_handling = SyncHandling::Genesis;
    let joiner_id = fixture
        .add_node(Arc::new(secret_key), config, storage_dir)
        .await;

    // Wait for joiner node to sync back to the block from era 1
    fixture
        .run_until(
            node_has_lowest_available_block_at_or_below_height(1, joiner_id),
            ONE_MIN,
        )
        .await;

    // Remove the weights for era 0 and era 1 from the validator matrix
    let runner = fixture
        .network
        .nodes_mut()
        .get_mut(&joiner_id)
        .expect("Could not find runner for node {joiner_id}");
    let reactor = runner.reactor_mut().inner_mut().inner_mut();
    reactor.validator_matrix.purge_era_validators(&ERA_ZERO);
    reactor.validator_matrix.purge_era_validators(&ERA_ONE);

    // Continue syncing and check if the joiner node reaches era 0
    fixture
        .run_until(
            node_has_lowest_available_block_at_or_below_height(0, joiner_id),
            ONE_MIN,
        )
        .await;
}

#[tokio::test]
async fn should_not_historical_sync_no_sync_node() {
    let initial_stakes = InitialStakes::Random { count: 5 };
    let spec_override = ChainspecOverride {
        minimum_block_time: "4seconds".parse().unwrap(),
        minimum_era_height: 1,
    };
    let mut fixture = TestFixture::new(initial_stakes, Some(spec_override)).await;

    // Wait for all nodes to complete block 1.
    fixture.run_until_block_height(1, ONE_MIN).await;

    // Create a joiner node.
    let highest_block = fixture.highest_complete_block();
    let trusted_hash = *highest_block.hash();
    let trusted_height = highest_block.height();
    assert!(
        trusted_height > 0,
        "trusted height must be non-zero to allow for checking that the joiner doesn't do \
        historical syncing"
    );
    info!("joining node using block {trusted_height} {trusted_hash}");
    let secret_key = SecretKey::random(&mut fixture.rng);
    let (mut config, storage_dir) = fixture.create_node_config(&secret_key, Some(trusted_hash));
    config.node.sync_handling = SyncHandling::NoSync;
    let joiner_id = fixture
        .add_node(Arc::new(secret_key), config, storage_dir)
        .await;

    let joiner_avail_range = |nodes: &Nodes| {
        nodes
            .get(&joiner_id)
            .expect("should have joiner")
            .main_reactor()
            .storage()
            .get_available_block_range()
    };

    // Run until the joiner doesn't have the default available block range, i.e. it has completed
    // syncing the initial block.
    fixture
        .try_run_until(
            |nodes: &Nodes| joiner_avail_range(nodes) != AvailableBlockRange::RANGE_0_0,
            ONE_MIN,
        )
        .await
        .expect("timed out waiting for joiner to sync first block");

    let available_block_range_pre = joiner_avail_range(fixture.network.nodes());

    let pre = available_block_range_pre.low();
    assert!(
        pre >= trusted_height,
        "should not have acquired a block earlier than trusted hash block {} {}",
        pre,
        trusted_height
    );

    // Ensure the joiner's chain is advancing.
    fixture
        .try_run_until(
            |nodes: &Nodes| joiner_avail_range(nodes).high() > available_block_range_pre.high(),
            ONE_MIN,
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "timed out waiting for joiner's highest complete block to exceed {}",
                available_block_range_pre.high()
            )
        });

    // Ensure the joiner is not doing historical sync.
    fixture
        .try_run_until(
            |nodes: &Nodes| joiner_avail_range(nodes).low() < available_block_range_pre.low(),
            TEN_SECS,
        )
        .await
        .unwrap_err();
}

#[tokio::test]
async fn run_equivocator_network() {
    let mut rng = crate::new_rng();

    let alice_secret_key = Arc::new(SecretKey::random(&mut rng));
    let alice_public_key = PublicKey::from(&*alice_secret_key);
    let bob_secret_key = Arc::new(SecretKey::random(&mut rng));
    let bob_public_key = PublicKey::from(&*bob_secret_key);
    let charlie_secret_key = Arc::new(SecretKey::random(&mut rng));
    let charlie_public_key = PublicKey::from(&*charlie_secret_key);

    let mut stakes = BTreeMap::new();
    stakes.insert(alice_public_key.clone(), U512::from(1));
    stakes.insert(bob_public_key.clone(), U512::from(1));
    stakes.insert(charlie_public_key, U512::from(u64::MAX));

    // Here's where things go wrong: Bob doesn't run a node at all, and Alice runs two!
    let secret_keys = vec![
        alice_secret_key.clone(),
        alice_secret_key,
        charlie_secret_key,
    ];

    // We configure the era to take 15 rounds. That should guarantee that the two nodes equivocate.
    let spec_override = ChainspecOverride {
        minimum_era_height: 10,
        ..Default::default()
    };

    let mut fixture =
        TestFixture::new_with_keys(rng, secret_keys, stakes.clone(), Some(spec_override)).await;

    let min_round_len = fixture.chainspec.core_config.minimum_block_time;
    let mut maybe_first_message_time = None;

    let mut alice_reactors = fixture
        .network
        .reactors_mut()
        .filter(|reactor| *reactor.inner().consensus().public_key() == alice_public_key);

    // Delay all messages to and from the first of Alice's nodes until three rounds after the first
    // message.  Further, significantly delay any incoming pings to avoid the node detecting the
    // doppelganger and deactivating itself.
    alice_reactors.next().unwrap().set_filter(move |event| {
        if is_ping(&event) {
            return Either::Left(time::sleep((min_round_len * 30).into()).event(move |_| event));
        }

        // Filter out all incoming and outgoing consensus message traffic.
        let now = Timestamp::now();
        match &event {
            MainEvent::ConsensusMessageIncoming(_) | MainEvent::ConsensusDemand(_) => {
                // delayed.
            }
            MainEvent::NetworkRequest(req) if matches!(req.payload(), Message::Consensus(_)) => {
                // delayed
            }
            _ => {
                return Either::Right(event);
            }
        };
        let first_message_time = *maybe_first_message_time.get_or_insert(now);
        if now < first_message_time + min_round_len * 3 {
            return Either::Left(time::sleep(min_round_len.into()).event(move |_| event));
        }
        Either::Right(event)
    });

    // Significantly delay all incoming pings to the second of Alice's nodes.
    alice_reactors.next().unwrap().set_filter(move |event| {
        if is_ping(&event) {
            return Either::Left(time::sleep((min_round_len * 30).into()).event(move |_| event));
        }
        Either::Right(event)
    });

    assert!(alice_reactors.next().is_none());
    drop(alice_reactors);

    let era_count = 4;

    let timeout = ONE_MIN * era_count as u32;
    info!("Waiting for {} eras to end.", era_count);
    fixture
        .run_until_stored_switch_block_header(EraId::new(era_count - 1), timeout)
        .await;

    let switch_blocks = SwitchBlocks::collect(fixture.network.nodes(), era_count);
    let bids: Vec<Bids> = (0..era_count)
        .map(|era_number| switch_blocks.bids(fixture.network.nodes(), era_number))
        .collect();

    // Since this setup sometimes produces no equivocation or an equivocation in era 2 rather than
    // era 1, we set an offset here.  If neither era has an equivocation, exit early.
    // TODO: Remove this once https://github.com/casper-network/casper-node/issues/1859 is fixed.
    for switch_block in &switch_blocks.headers {
        let era_id = switch_block.era_id();
        let count = switch_blocks.equivocators(era_id.value()).len();
        info!("equivocators in {}: {}", era_id, count);
    }
    let offset = if !switch_blocks.equivocators(1).is_empty() {
        0
    } else if !switch_blocks.equivocators(2).is_empty() {
        error!("failed to equivocate in era 1 - asserting equivocation detected in era 2");
        1
    } else {
        error!("failed to equivocate in era 1 or 2");
        return;
    };

    // Era 0 consists only of the genesis block.
    // In era 1, Alice equivocates. Since eviction takes place with a delay of one
    // (`auction_delay`) era, she is still included in the next era's validator set.
    assert_eq!(
        switch_blocks.equivocators(1 + offset),
        [alice_public_key.clone()]
    );
    assert!(bids[1 + offset as usize][&alice_public_key].inactive());
    assert!(switch_blocks
        .next_era_validators(1 + offset)
        .contains_key(&alice_public_key));

    // In era 2 Alice is banned. Banned validators count neither as faulty nor inactive, even
    // though they cannot participate. In the next era, she will be evicted.
    assert_eq!(switch_blocks.equivocators(2 + offset), []);
    assert!(bids[2 + offset as usize][&alice_public_key].inactive());
    assert!(!switch_blocks
        .next_era_validators(2 + offset)
        .contains_key(&alice_public_key));

    // In era 3 she is not a validator anymore and her bid remains deactivated.
    if offset == 0 {
        assert_eq!(switch_blocks.equivocators(3), []);
        assert!(bids[3][&alice_public_key].inactive());
        assert!(!switch_blocks
            .next_era_validators(3)
            .contains_key(&alice_public_key));
    }

    // Bob is inactive.
    assert_eq!(
        switch_blocks.inactive_validators(1),
        [bob_public_key.clone()]
    );
    assert_eq!(
        switch_blocks.inactive_validators(2),
        [bob_public_key.clone()]
    );

    // We don't slash, so the stakes are never reduced.
    for (public_key, stake) in &stakes {
        assert!(bids[0][public_key].staked_amount() >= stake);
        assert!(bids[1][public_key].staked_amount() >= stake);
        assert!(bids[2][public_key].staked_amount() >= stake);
        assert!(bids[3][public_key].staked_amount() >= stake);
    }
}

async fn assert_network_shutdown_for_upgrade_with_stakes(initial_stakes: InitialStakes) {
    let mut fixture = TestFixture::new(initial_stakes, None).await;

    // An upgrade is scheduled for era 2, after the switch block in era 1 (height 2).
    fixture.schedule_upgrade_for_era_two().await;

    // Run until the nodes shut down for the upgrade.
    fixture
        .network
        .settle_on_exit(&mut fixture.rng, ExitCode::Success, ONE_MIN)
        .await;
}

#[tokio::test]
async fn nodes_should_have_enough_signatures_before_upgrade_with_equal_stake() {
    // Equal stake ensures that one node was able to learn about signatures created by the other, by
    // whatever means necessary (gossiping, broadcasting, fetching, etc.).
    let initial_stakes = InitialStakes::AllEqual {
        count: 2,
        stake: u128::MAX,
    };
    assert_network_shutdown_for_upgrade_with_stakes(initial_stakes).await;
}

#[tokio::test]
async fn nodes_should_have_enough_signatures_before_upgrade_with_one_dominant_stake() {
    let initial_stakes = InitialStakes::FromVec(vec![u128::MAX, 255]);
    assert_network_shutdown_for_upgrade_with_stakes(initial_stakes).await;
}

#[tokio::test]
async fn dont_upgrade_without_switch_block() {
    let initial_stakes = InitialStakes::Random { count: 2 };
    let mut fixture = TestFixture::new(initial_stakes, None).await;
    fixture.run_until_consensus_in_era(ERA_ONE, ONE_MIN).await;

    eprintln!(
        "Running 'dont_upgrade_without_switch_block' test with rng={}",
        fixture.rng
    );

    // An upgrade is scheduled for era 2, after the switch block in era 1 (height 2).
    // We artificially delay the execution of that block.
    fixture.schedule_upgrade_for_era_two().await;
    for runner in fixture.network.runners_mut() {
        let mut exec_request_received = false;
        runner.reactor_mut().inner_mut().set_filter(move |event| {
            if let MainEvent::ContractRuntimeRequest(
                ContractRuntimeRequest::EnqueueBlockForExecution {
                    finalized_block, ..
                },
            ) = &event
            {
                if finalized_block.era_report().is_some()
                    && finalized_block.era_id() == ERA_ONE
                    && !exec_request_received
                {
                    info!("delaying {}", finalized_block);
                    exec_request_received = true;
                    return Either::Left(
                        time::sleep(Duration::from_secs(10)).event(move |_| event),
                    );
                }
                info!("not delaying {}", finalized_block);
            }
            Either::Right(event)
        });
    }

    // Run until the nodes shut down for the upgrade.
    fixture
        .network
        .settle_on_exit(&mut fixture.rng, ExitCode::Success, ONE_MIN)
        .await;

    // Verify that the switch block has been stored: Even though it was delayed the node didn't
    // restart before executing and storing it.
    for runner in fixture.network.nodes().values() {
        let header = runner
            .main_reactor()
            .storage()
            .read_block_by_height(2)
            .expect("failed to read from storage")
            .expect("missing switch block")
            .take_header();
        assert_eq!(ERA_ONE, header.era_id(), "era should be 1");
        assert!(header.is_switch_block(), "header should be switch block");
    }
}

#[tokio::test]
async fn should_store_finalized_approvals() {
    // Set up a network with two nodes where node 0 (Alice) is effectively guaranteed to be the
    // proposer.
    let initial_stakes = InitialStakes::FromVec(vec![u128::MAX, 1]);
    let mut fixture = TestFixture::new(initial_stakes, None).await;

    let alice_secret_key = Arc::clone(&fixture.node_contexts[0].secret_key);
    let alice_public_key = PublicKey::from(&*alice_secret_key);
    let bob_secret_key = Arc::clone(&fixture.node_contexts[1].secret_key);
    let charlie_secret_key = Arc::new(SecretKey::random(&mut fixture.rng)); // just for ordering testing purposes

    // Wait for all nodes to complete era 0.
    fixture.run_until_consensus_in_era(ERA_ONE, ONE_MIN).await;

    // Submit a deploy.
    let mut deploy_alice_bob = Deploy::random_valid_native_transfer_without_deps(&mut fixture.rng);
    let mut deploy_alice_bob_charlie = deploy_alice_bob.clone();
    let mut deploy_bob_alice = deploy_alice_bob.clone();

    deploy_alice_bob.sign(&alice_secret_key);
    deploy_alice_bob.sign(&bob_secret_key);

    deploy_alice_bob_charlie.sign(&alice_secret_key);
    deploy_alice_bob_charlie.sign(&bob_secret_key);
    deploy_alice_bob_charlie.sign(&charlie_secret_key);

    deploy_bob_alice.sign(&bob_secret_key);
    deploy_bob_alice.sign(&alice_secret_key);

    // We will be testing the correct sequence of approvals against the deploy signed by Bob and
    // Alice.
    // The deploy signed by Alice and Bob should give the same ordering of approvals.
    let expected_approvals: Vec<_> = deploy_bob_alice.approvals().iter().cloned().collect();

    // We'll give the deploy signed by Alice, Bob and Charlie to Bob, so these will be his original
    // approvals. Save these for checks later.
    let bobs_original_approvals: Vec<_> = deploy_alice_bob_charlie
        .approvals()
        .iter()
        .cloned()
        .collect();
    assert_ne!(bobs_original_approvals, expected_approvals);

    let deploy_hash = *deploy_alice_bob.deploy_or_transfer_hash().deploy_hash();

    for runner in fixture.network.runners_mut() {
        if runner.main_reactor().consensus().public_key() == &alice_public_key {
            // Alice will propose the deploy signed by Alice and Bob.
            runner
                .process_injected_effects(|effect_builder| {
                    effect_builder
                        .put_deploy_to_storage(Arc::new(deploy_alice_bob.clone()))
                        .ignore()
                })
                .await;
            runner
                .process_injected_effects(|effect_builder| {
                    effect_builder
                        .announce_new_deploy_accepted(
                            Arc::new(deploy_alice_bob.clone()),
                            Source::Client,
                        )
                        .ignore()
                })
                .await;
        } else {
            // Bob will receive the deploy signed by Alice, Bob and Charlie.
            runner
                .process_injected_effects(|effect_builder| {
                    effect_builder
                        .put_deploy_to_storage(Arc::new(deploy_alice_bob_charlie.clone()))
                        .ignore()
                })
                .await;
            runner
                .process_injected_effects(|effect_builder| {
                    effect_builder
                        .announce_new_deploy_accepted(
                            Arc::new(deploy_alice_bob_charlie.clone()),
                            Source::Client,
                        )
                        .ignore()
                })
                .await;
        }
    }

    // Run until the deploy gets executed.
    let has_stored_exec_results = |nodes: &Nodes| {
        nodes.values().all(|runner| {
            runner
                .main_reactor()
                .storage()
                .get_deploy_metadata_by_hash(&deploy_hash)
                .is_some()
        })
    };
    fixture.run_until(has_stored_exec_results, ONE_MIN).await;

    // Check if the approvals agree.
    for runner in fixture.network.nodes().values() {
        let maybe_dwa = runner
            .main_reactor()
            .storage()
            .get_deploy_with_finalized_approvals_by_hash(&deploy_hash);
        let maybe_finalized_approvals = maybe_dwa
            .as_ref()
            .and_then(|dwa| dwa.finalized_approvals())
            .map(|fa| fa.inner().iter().cloned().collect());
        let maybe_original_approvals = maybe_dwa
            .as_ref()
            .map(|dwa| dwa.original_approvals().iter().cloned().collect());
        if runner.main_reactor().consensus().public_key() != &alice_public_key {
            // Bob should have finalized approvals, and his original approvals should be different.
            assert_eq!(
                maybe_finalized_approvals.as_ref(),
                Some(&expected_approvals)
            );
            assert_eq!(
                maybe_original_approvals.as_ref(),
                Some(&bobs_original_approvals)
            );
        } else {
            // Alice should only have the correct approvals as the original ones, and no finalized
            // approvals (as they wouldn't be stored, because they would be the same as the
            // original ones).
            assert_eq!(maybe_finalized_approvals.as_ref(), None);
            assert_eq!(maybe_original_approvals.as_ref(), Some(&expected_approvals));
        }
    }
}

// This test exercises a scenario in which a proposed block contains invalid accusations.
// Blocks containing no deploys or transfers used to be incorrectly marked as not needing
// validation even if they contained accusations, which opened up a security hole through which a
// malicious validator could accuse whomever they wanted of equivocating and have these
// accusations accepted by the other validators. This has been patched and the test asserts that
// such a scenario is no longer possible.
#[tokio::test]
async fn empty_block_validation_regression() {
    let initial_stakes = InitialStakes::AllEqual {
        count: 4,
        stake: 100,
    };
    let spec_override = ChainspecOverride {
        minimum_era_height: 15,
        ..Default::default()
    };
    let mut fixture = TestFixture::new(initial_stakes, Some(spec_override)).await;

    let malicious_validator =
        PublicKey::from(fixture.node_contexts.first().unwrap().secret_key.as_ref());
    info!("Malicious validator: {}", malicious_validator);
    let everyone_else: Vec<_> = fixture
        .node_contexts
        .iter()
        .skip(1)
        .map(|node_context| PublicKey::from(node_context.secret_key.as_ref()))
        .collect();
    let malicious_id = fixture.node_contexts.first().unwrap().id;
    let malicious_runner = fixture.network.nodes_mut().get_mut(&malicious_id).unwrap();
    malicious_runner
        .reactor_mut()
        .inner_mut()
        .set_filter(move |event| match event {
            MainEvent::Consensus(consensus::Event::NewBlockPayload(NewBlockPayload {
                era_id,
                block_payload: _,
                block_context,
            })) => {
                info!("Accusing everyone else!");
                // We hook into the NewBlockPayload event to replace the block being proposed with
                // an empty one that accuses all the validators, except the malicious validator.
                Either::Right(MainEvent::Consensus(consensus::Event::NewBlockPayload(
                    NewBlockPayload {
                        era_id,
                        block_payload: Arc::new(BlockPayload::new(
                            vec![],
                            vec![],
                            everyone_else.clone(),
                            false,
                        )),
                        block_context,
                    },
                )))
            }
            event => Either::Right(event),
        });

    info!("Waiting for the first era after genesis to end.");
    fixture.run_until_consensus_in_era(ERA_TWO, ONE_MIN).await;
    let switch_blocks = SwitchBlocks::collect(fixture.network.nodes(), 2);

    // Nobody actually double-signed. The accusations should have had no effect.
    assert_eq!(
        switch_blocks.equivocators(0),
        [],
        "expected no equivocators"
    );
    // If the malicious validator was the first proposer, all their Highway units might be invalid,
    // because they all refer to the invalid proposal, so they might get flagged as inactive. No
    // other validators should be considered inactive.
    match switch_blocks.inactive_validators(0) {
        [] => {}
        [inactive_validator] if malicious_validator == *inactive_validator => {}
        inactive => panic!("unexpected inactive validators: {:?}", inactive),
    }
}

#[tokio::test]
#[ignore] // Disabled until fixed, after the issue with `TestFixture` and multiple `TestRng`s was fixed.
async fn all_metrics_from_1_5_are_present() {
    testing::init_logging();

    let mut fixture = TestFixture::new(
        InitialStakes::AllEqual {
            count: 4,
            stake: 100,
        },
        None,
    )
    .await;
    let mut rng = fixture.rng_mut().create_child();

    let net = fixture.network_mut();

    net.settle_on_component_state(
        &mut rng,
        "rest_server",
        &ComponentState::Initialized,
        Duration::from_secs(59),
    )
    .await;

    // Get the node ID.
    let node_id = *net.nodes().keys().next().unwrap();

    let rest_addr = net.nodes()[&node_id]
        .main_reactor()
        .rest_server
        .bind_address();

    // We let the entire network run in the background, until our request completes.
    let finish_cranking = fixture.run_until_stopped(rng);

    let metrics_response = reqwest::Client::builder()
        .build()
        .expect("failed to build client")
        .get(format!("http://localhost:{}/metrics", rest_addr.port()))
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .expect("request failed")
        .error_for_status()
        .expect("error response on metrics request")
        .text()
        .await
        .expect("error retrieving text on metrics request");

    let (_net, _rng) = finish_cranking.await;

    let actual = extract_metric_names(&metrics_response);
    let raw_1_5 = fs::read_to_string(RESOURCES_PATH.join("metrics-1.5.txt"))
        .expect("could not read 1.5 metrics snapshot");
    let metrics_1_5 = extract_metric_names(&raw_1_5);

    let missing: HashSet<_> = metrics_1_5.difference(&actual).collect();
    assert!(
        missing.is_empty(),
        "missing 1.5 metrics in current metrics set: {:?}",
        missing
    );
}

#[tokio::test]
#[ignore] // Disabled, until the issue with `TestFixture` and multiple `TestRng`s is fixed.
async fn port_bound_components_report_ready() {
    testing::init_logging();

    let mut rng = crate::new_rng();

    let mut fixture = TestFixture::new(
        InitialStakes::AllEqual {
            count: 4,
            stake: 100,
        },
        None,
    )
    .await;
    let net = fixture.network_mut();

    // Ensure all `PortBoundComponent` implementors report readiness eventually.
    net.settle_on_component_state(
        &mut rng,
        "rest_server",
        &ComponentState::Initialized,
        Duration::from_secs(10),
    )
    .await;

    net.settle_on_component_state(
        &mut rng,
        "rpc_server",
        &ComponentState::Initialized,
        Duration::from_secs(10),
    )
    .await;

    net.settle_on_component_state(
        &mut rng,
        "event_stream_server",
        &ComponentState::Initialized,
        Duration::from_secs(10),
    )
    .await;

    net.settle_on_component_state(
        &mut rng,
        "diagnostics_port",
        &ComponentState::Initialized,
        Duration::from_secs(10),
    )
    .await;
}

#[tokio::test]
async fn network_should_recover_from_stall() {
    // Set up a network with three nodes.
    let initial_stakes = InitialStakes::AllEqual {
        count: 3,
        stake: 100,
    };
    let mut fixture = TestFixture::new(initial_stakes, None).await;

    // Let all nodes progress until block 2 is marked complete.
    fixture.run_until_block_height(2, ONE_MIN).await;

    // Kill all nodes except for node 0.
    let mut stopped_nodes = vec![];
    for _ in 1..fixture.node_contexts.len() {
        let node_context = fixture.remove_and_stop_node(1);
        stopped_nodes.push(node_context);
    }

    // Expect node 0 can't produce more blocks, i.e. the network has stalled.
    fixture
        .try_run_until_block_height(3, TEN_SECS)
        .await
        .expect_err("should time out");

    // Restart the stopped nodes.
    for node_context in stopped_nodes {
        fixture
            .add_node(
                node_context.secret_key,
                node_context.config,
                node_context.storage_dir,
            )
            .await;
    }

    // Ensure all nodes progress until block 3 is marked complete.
    fixture.run_until_block_height(3, TEN_SECS).await;
}
