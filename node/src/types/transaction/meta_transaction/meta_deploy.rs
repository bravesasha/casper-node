use casper_types::{
    Deploy, ExecutableDeployItem, InvalidDeploy, InvalidTransaction, TransactionLaneDefinition,
    TransactionV1Config, MINT_LANE_ID,
};
use datasize::DataSize;
use serde::Serialize;

#[derive(Clone, Debug, Serialize, DataSize)]
pub(crate) struct MetaDeploy {
    deploy: Deploy,
    //When a deploy is a WASM we categorize it as "largest wasm possible".
    //We need to keep that id here since we can fetch it only from chainspec.
    largest_wasm_id: u8,
}

impl MetaDeploy {
    pub(crate) fn from_deploy(
        deploy: Deploy,
        config: &TransactionV1Config,
    ) -> Result<Self, InvalidTransaction> {
        let maybe_biggest_lane_limit = Self::calculate_lane_id_of_biggest_wasm(config.wasm_lanes());
        if let Some(largest_wasm_id) = maybe_biggest_lane_limit {
            Ok(MetaDeploy {
                deploy,
                largest_wasm_id,
            })
        } else {
            // Seems like chainspec didn't have any wasm lanes configured
            Err(InvalidTransaction::Deploy(
                InvalidDeploy::InvalidChainspecConfiguration,
            ))
        }
    }

    pub(crate) fn lane_id(&self) -> u8 {
        if self.deploy.is_transfer() {
            MINT_LANE_ID
        } else {
            self.largest_wasm_id
        }
    }

    fn calculate_lane_id_of_biggest_wasm(wasm_lanes: &[TransactionLaneDefinition]) -> Option<u8> {
        wasm_lanes
            .iter()
            .max_by(|left, right| {
                left.max_transaction_length
                    .cmp(&right.max_transaction_length)
            })
            .map(|definition| definition.id)
    }

    pub(crate) fn session(&self) -> &ExecutableDeployItem {
        self.deploy.session()
    }

    pub(crate) fn deploy(&self) -> &Deploy {
        &self.deploy
    }
}

#[cfg(test)]
mod tests {
    use super::MetaDeploy;
    use casper_types::TransactionLaneDefinition;
    #[test]
    fn calculate_lane_id_of_biggest_wasm_should_return_none_on_empty() {
        let wasms = vec![];
        assert!(MetaDeploy::calculate_lane_id_of_biggest_wasm(&wasms).is_none());
    }

    #[test]
    fn calculate_lane_id_of_biggest_wasm_should_return_biggest() {
        let wasms = vec![
            TransactionLaneDefinition {
                id: 0,
                max_transaction_length: 1,
                max_transaction_args_length: 2,
                max_transaction_gas_limit: 3,
                max_transaction_count: 4,
            },
            TransactionLaneDefinition {
                id: 1,
                max_transaction_length: 10,
                max_transaction_args_length: 2,
                max_transaction_gas_limit: 3,
                max_transaction_count: 4,
            },
        ];
        assert_eq!(
            MetaDeploy::calculate_lane_id_of_biggest_wasm(&wasms),
            Some(1)
        );
        let wasms = vec![
            TransactionLaneDefinition {
                id: 0,
                max_transaction_length: 1,
                max_transaction_args_length: 2,
                max_transaction_gas_limit: 3,
                max_transaction_count: 4,
            },
            TransactionLaneDefinition {
                id: 1,
                max_transaction_length: 10,
                max_transaction_args_length: 2,
                max_transaction_gas_limit: 3,
                max_transaction_count: 4,
            },
            TransactionLaneDefinition {
                id: 2,
                max_transaction_length: 7,
                max_transaction_args_length: 2,
                max_transaction_gas_limit: 3,
                max_transaction_count: 4,
            },
        ];
        assert_eq!(
            MetaDeploy::calculate_lane_id_of_biggest_wasm(&wasms),
            Some(1)
        );

        let wasms = vec![
            TransactionLaneDefinition {
                id: 0,
                max_transaction_length: 1,
                max_transaction_args_length: 2,
                max_transaction_gas_limit: 3,
                max_transaction_count: 4,
            },
            TransactionLaneDefinition {
                id: 1,
                max_transaction_length: 10,
                max_transaction_args_length: 2,
                max_transaction_gas_limit: 3,
                max_transaction_count: 4,
            },
            TransactionLaneDefinition {
                id: 2,
                max_transaction_length: 70,
                max_transaction_args_length: 2,
                max_transaction_gas_limit: 3,
                max_transaction_count: 4,
            },
        ];
        assert_eq!(
            MetaDeploy::calculate_lane_id_of_biggest_wasm(&wasms),
            Some(2)
        );
    }
}
