//! This module is responsible for resolving host functions from within the WASM engine.
pub mod error;
pub mod memory_resolver;
pub(crate) mod v1_function_index;
mod v1_resolver;

use casper_wasmi::ModuleImportResolver;

use casper_types::ProtocolVersion;

use self::error::ResolverError;
use super::engine_state::EngineConfig;
use crate::resolvers::memory_resolver::MemoryResolver;

/// Creates a module resolver for given protocol version.
///
/// * `protocol_version` Version of the protocol. Can't be lower than 1.
pub(crate) fn create_module_resolver(
    _protocol_version: ProtocolVersion,
    engine_config: &EngineConfig,
) -> Result<impl ModuleImportResolver + MemoryResolver, ResolverError> {
    Ok(v1_resolver::RuntimeModuleImportResolver::new(
        engine_config.wasm_config().v1().max_memory(),
    ))
    // if in future it is necessary to pick a different resolver
    // based on the protocol version, modify this logic accordingly
    // if there is an unsupported / unknown protocol version return the following error:
    // Err(ResolverError::UnknownProtocolVersion(protocol_version))
}

#[cfg(test)]
mod tests {
    use casper_types::ProtocolVersion;

    use super::*;

    #[test]
    fn resolve_invalid_module() {
        // NOTE: we are currently not enforcing underlying logic
        assert!(
            create_module_resolver(ProtocolVersion::default(), &EngineConfig::default()).is_ok()
        );
    }

    #[test]
    fn protocol_version_1_always_resolves() {
        assert!(create_module_resolver(ProtocolVersion::V1_0_0, &EngineConfig::default()).is_ok());
    }
}
