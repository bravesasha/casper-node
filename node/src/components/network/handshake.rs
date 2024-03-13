//! Handshake handling for `small_network`.
//!
//! The handshake differs from the rest of the networking code since it is (almost) unmodified since
//! version 1.0, to allow nodes to make informed decisions about blocking other nodes.
//!
//! This module contains an implementation for a minimal framing format based on 32-bit fixed size
//! big endian length prefixes.

use std::net::SocketAddr;

use casper_types::PublicKey;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use serde::{de::DeserializeOwned, Serialize};
use tracing::debug;

use super::{
    chain_info::ChainInfo,
    connection_id::ConnectionId,
    error::{ConnectionError, RawFrameIoError},
    message::NodeKeyPair,
    Message, Transport,
};

/// The outcome of the handshake process.
pub(crate) struct HandshakeOutcome {
    /// A framed transport for peer.
    pub(crate) transport: Transport,
    /// Public address advertised by the peer.
    pub(crate) public_addr: SocketAddr,
    /// The public key the peer is validating with, if any.
    pub(crate) peer_consensus_public_key: Option<Box<PublicKey>>,
}

/// Reads a 32 byte big endian integer prefix, followed by an actual raw message.
async fn read_length_prefixed_frame<R>(
    max_length: u32,
    stream: &mut R,
) -> Result<Vec<u8>, RawFrameIoError>
where
    R: AsyncRead + Unpin,
{
    let mut length_prefix_raw: [u8; 4] = [0; 4];
    stream
        .read_exact(&mut length_prefix_raw)
        .await
        .map_err(RawFrameIoError::Io)?;

    let length = u32::from_be_bytes(length_prefix_raw);

    if length > max_length {
        return Err(RawFrameIoError::MaximumLengthExceeded(length as usize));
    }

    let mut raw = Vec::new(); // not preallocating, to make DOS attacks harder.

    // We can now read the raw frame and return.
    stream
        .take(length as u64)
        .read_to_end(&mut raw)
        .await
        .map_err(RawFrameIoError::Io)?;

    Ok(raw)
}

/// Writes data to an async writer, prefixing it with the 32 bytes big endian message length.
///
/// Output will be flushed after sending.
async fn write_length_prefixed_frame<W>(stream: &mut W, data: &[u8]) -> Result<(), RawFrameIoError>
where
    W: AsyncWrite + Unpin,
{
    if data.len() > u32::MAX as usize {
        return Err(RawFrameIoError::MaximumLengthExceeded(data.len()));
    }

    async move {
        stream.write_all(&(data.len() as u32).to_be_bytes()).await?;
        stream.write_all(data).await?;
        stream.flush().await?;
        Ok(())
    }
    .await
    .map_err(RawFrameIoError::Io)?;

    Ok(())
}

/// Serializes an item with the encoding settings specified for handshakes.
pub(crate) fn serialize<T>(item: &T) -> Result<Vec<u8>, rmp_serde::encode::Error>
where
    T: Serialize,
{
    rmp_serde::to_vec(item)
}

/// Deserialize an item with the encoding settings specified for handshakes.
pub(crate) fn deserialize<T>(raw: &[u8]) -> Result<T, rmp_serde::decode::Error>
where
    T: DeserializeOwned,
{
    rmp_serde::from_slice(raw)
}

/// Data necessary to perform a handshake.
#[derive(Debug)]
pub(crate) struct HandshakeConfiguration {
    /// Chain info extract from chainspec.
    chain_info: ChainInfo,
    /// Optional set of signing keys, to identify as a node during handshake.
    node_key_pair: Option<NodeKeyPair>,
    /// Our own public listening address.
    public_addr: SocketAddr,
}

impl HandshakeConfiguration {
    /// Creates a new handshake configuration.
    pub(crate) fn new(
        chain_info: ChainInfo,
        node_key_pair: Option<NodeKeyPair>,
        public_addr: SocketAddr,
    ) -> Self {
        Self {
            chain_info,
            node_key_pair,
            public_addr,
        }
    }

    /// Performs a handshake.
    ///
    /// This function is cancellation safe.
    pub(crate) async fn negotiate_handshake(
        &self,
        transport: Transport,
    ) -> Result<HandshakeOutcome, ConnectionError> {
        let connection_id = ConnectionId::from_connection(transport.ssl());

        // Manually encode a handshake.
        let handshake_message = self.chain_info.create_handshake(
            self.public_addr,
            self.node_key_pair.as_ref(),
            connection_id,
        );

        let serialized_handshake_message =
            serialize(&handshake_message).map_err(ConnectionError::CouldNotEncodeOurHandshake)?;

        // To ensure we are not dead-locking, we split the transport here and send the handshake in
        // a background task before awaiting one ourselves. This ensures we can make progress
        // regardless of the size of the outgoing handshake.
        let (mut read_half, mut write_half) = tokio::io::split(transport);

        // TODO: This need not be spawned, but could be a local futures unordered.
        let handshake_send = tokio::spawn(async move {
            write_length_prefixed_frame(&mut write_half, &serialized_handshake_message).await?;
            Ok::<_, RawFrameIoError>(write_half)
        });

        // The remote's message should be a handshake, but can technically be any message. We
        // receive, deserialize and check it.
        let remote_message_raw = read_length_prefixed_frame(
            self.chain_info.maximum_handshake_message_size,
            &mut read_half,
        )
        .await
        .map_err(ConnectionError::HandshakeRecv)?;

        // Ensure the handshake was sent correctly.
        let write_half = handshake_send
            .await
            .map_err(ConnectionError::HandshakeSenderCrashed)?
            .map_err(ConnectionError::HandshakeSend)?;

        let remote_message: Message<()> = deserialize(&remote_message_raw)
            .map_err(ConnectionError::InvalidRemoteHandshakeMessage)?;

        if let Message::<()>::Handshake {
            network_name,
            public_addr,
            protocol_version,
            consensus_certificate,
            chainspec_hash,
        } = remote_message
        {
            debug!(%protocol_version, "handshake received");

            // The handshake was valid, we can check the network name.
            if network_name != self.chain_info.network_name {
                return Err(ConnectionError::WrongNetwork(network_name));
            }

            // If there is a version mismatch, we treat it as a connection error. We do not ban
            // peers for this error, but instead rely on exponential backoff, as bans would result
            // in issues during upgrades where nodes may have a legitimate reason for differing
            // versions.
            //
            // Since we are not using SemVer for versioning, we cannot make any assumptions about
            // compatibility, so we allow only exact version matches.
            if protocol_version != self.chain_info.protocol_version {
                return Err(ConnectionError::IncompatibleVersion(protocol_version));
            }

            // We check the chainspec hash to ensure peer is using the same chainspec as us.
            // The remote message should always have a chainspec hash at this point since
            // we checked the protocol version previously.
            let peer_chainspec_hash =
                chainspec_hash.ok_or(ConnectionError::MissingChainspecHash)?;
            if peer_chainspec_hash != self.chain_info.chainspec_hash {
                return Err(ConnectionError::WrongChainspecHash(peer_chainspec_hash));
            }

            let peer_consensus_public_key = consensus_certificate
                .map(|cert| {
                    cert.validate(connection_id)
                        .map_err(ConnectionError::InvalidConsensusCertificate)
                })
                .transpose()?
                .map(Box::new);

            let transport = read_half.unsplit(write_half);

            Ok(HandshakeOutcome {
                transport,
                public_addr,
                peer_consensus_public_key,
            })
        } else {
            // Received a non-handshake, this is an error.
            Err(ConnectionError::DidNotSendHandshake)
        }
    }
}
