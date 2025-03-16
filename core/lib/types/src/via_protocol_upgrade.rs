use zksync_basic_types::H256;
use zksync_contracts::deployer_contract;

use crate::{ethabi::Token, Address, U256};

#[derive(Debug, Clone)]
pub struct ForceDeployment {
    // The bytecode hash to put on an address
    pub bytecode_hash: H256,
    // The address on which to deploy the bytecode hash to
    pub address: Address,
    // Whether to run the constructor on the force deployment
    pub call_constructor: bool,
    // The value with which to initialize a contract
    pub value: U256,
    // The constructor calldata
    pub input: Vec<u8>,
}

impl ForceDeployment {
    pub fn calldata(system_contracts: Vec<(Address, H256)>) -> anyhow::Result<Vec<u8>> {
        let mut deployment = Vec::with_capacity(system_contracts.len());
        for (address, bytecode_hash) in system_contracts {
            deployment.push(ForceDeployment {
                bytecode_hash,
                address,
                call_constructor: false,
                value: U256::zero(),
                input: vec![],
            });
        }

        let encoded_deployments: Vec<_> = deployment
            .iter()
            .map(|deployment| {
                Token::Tuple(vec![
                    Token::FixedBytes(deployment.bytecode_hash.as_bytes().to_vec()),
                    Token::Address(deployment.address),
                    Token::Bool(deployment.call_constructor),
                    Token::Uint(deployment.value),
                    Token::Bytes(deployment.input.clone()),
                ])
            })
            .collect();

        let calldata = deployer_contract()
            .function("forceDeployOnAddresses")?
            .encode_input(&[Token::Array(encoded_deployments)])?;
        Ok(calldata)
    }
}
