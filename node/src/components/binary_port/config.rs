use std::str::FromStr;

use casper_types::TimeDiff;
use datasize::DataSize;
use serde::{Deserialize, Serialize};

/// Uses a fixed port per node, but binds on any interface.
const DEFAULT_ADDRESS: &str = "0.0.0.0:0";
/// Default maximum message size.
const DEFAULT_MAX_MESSAGE_SIZE: u32 = 4 * 1024 * 1024;
/// Default maximum number of connections.
const DEFAULT_MAX_CONNECTIONS: usize = 5;
/// Default maximum number of requests per second.
const DEFAULT_QPS_LIMIT: usize = 110;
/// Default interval between connection keepalive checks.
const DEFAULT_KEEPALIVE_CHECK_INTERVAL: &str = "10sec";
/// Default amount of time to wait for activity on a connection before considering it stale.
const DEFAULT_KEEPALIVE_NO_ACTIVITY_TIMEOUT: &str = "120sec";

/// Binary port server configuration.
#[derive(Clone, DataSize, Debug, Deserialize, Serialize)]
// Disallow unknown fields to ensure config files and command-line overrides contain valid keys.
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Setting to enable the BinaryPort server.
    pub enable_server: bool,
    /// Address to bind BinaryPort server to.
    pub address: String,
    /// Flag used to enable/disable the [`AllValues`] request
    // In case we need "enabled" flag for more than 2 requests we should introduce generic
    // "function disabled/enabled" mechanism. For now, we can stick to these two booleans.
    pub allow_request_get_all_values: bool,
    /// Flag used to enable/disable the [`Trie`] request
    pub allow_request_get_trie: bool,
    /// Flag used to enable/disable the [`TrySpeculativeExec`] request.
    pub allow_request_speculative_exec: bool,
    /// Maximum size of the binary port message.
    pub max_message_size_bytes: u32,
    /// Maximum number of connections to the server.
    pub max_connections: usize,
    /// Maximum number of requests per second.
    pub qps_limit: usize,
    /// Time of interval between keepalive checks for a binary port connection.
    pub keepalive_check_interval: TimeDiff,
    /// Duration of time the keepalive mechanism waits for activity on a binary port connection
    /// before considering it stale and closing it.
    pub keepalive_no_activity_timeout: TimeDiff,
}

impl Config {
    /// Creates a default instance for `BinaryPort`.
    pub fn new() -> Self {
        Config {
            enable_server: true,
            address: DEFAULT_ADDRESS.to_string(),
            allow_request_get_all_values: false,
            allow_request_get_trie: false,
            allow_request_speculative_exec: false,
            max_message_size_bytes: DEFAULT_MAX_MESSAGE_SIZE,
            max_connections: DEFAULT_MAX_CONNECTIONS,
            qps_limit: DEFAULT_QPS_LIMIT,
            keepalive_check_interval: TimeDiff::from_str(DEFAULT_KEEPALIVE_CHECK_INTERVAL).unwrap(),
            keepalive_no_activity_timeout: TimeDiff::from_str(
                DEFAULT_KEEPALIVE_NO_ACTIVITY_TIMEOUT,
            )
            .unwrap(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config::new()
    }
}
