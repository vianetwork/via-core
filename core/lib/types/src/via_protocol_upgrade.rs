use zksync_basic_types::H256;
use zksync_contracts::deployer_contract;

use crate::{ethabi::Token, Address, U256};

pub fn get_calldata(system_contracts: Vec<(Address, H256)>) -> anyhow::Result<Vec<u8>> {
    let encoded_deployments: Vec<_> = system_contracts
        .into_iter()
        .map(|(address, bytecode_hash)| {
            Token::Tuple(vec![
                Token::FixedBytes(bytecode_hash.as_bytes().to_vec()),
                Token::Address(address),
                Token::Bool(false),
                Token::Uint(U256::zero()),
                Token::Bytes(vec![]),
            ])
        })
        .collect();

    Ok(deployer_contract()
        .function("forceDeployOnAddresses")?
        .encode_input(&[Token::Array(encoded_deployments)])?)
}
