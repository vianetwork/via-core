use std::str::FromStr;

use circuit_definitions::{
    circuit_definitions::aux_layer::ZkSyncSnarkWrapperCircuit,
    snark_wrapper::franklin_crypto::bellman::bn256::Bn256,
};
use ethers::{
    abi::{ethabi, ethereum_types, Abi, Function, Token},
    contract::BaseContract,
    providers::{Http, Middleware, Provider},
    types::TxHash,
};
use once_cell::sync::Lazy;
use primitive_types::{H256, U256};
use reqwest::StatusCode;
use via_verification::version_27::{
    crypto::deserialize_proof, errors::VerificationError, proof::ViaZKProof, types::BatchL1Data,
};

use crate::types::{JSONL2RPCResponse, JSONL2SyncRPCResponse, L1BatchRangeJson};

static BLOCK_COMMIT_EVENT_SIGNATURE: Lazy<H256> = Lazy::new(|| {
    ethabi::long_signature(
        "BlockCommit",
        &[
            ethabi::ParamType::Uint(256),
            ethabi::ParamType::FixedBytes(32),
            ethabi::ParamType::FixedBytes(32),
        ],
    )
});

// Fetches given batch information from Era RPC
pub async fn fetch_batch_protocol_version(batch_number: u64) -> Result<String, VerificationError> {
    tracing::info!(
        "Fetching batch {} protocol version from zkSync Era mainnet",
        batch_number
    );

    let domain = "https://mainnet.era.zksync.io";

    let client = reqwest::Client::new();

    let response = client
        .post(domain)
        .header("Content-Type", "application/json")
        .body(format!(
            r#"{{
            "jsonrpc": "2.0",
            "method": "zks_getL1BatchBlockRange",
            "params": [{}],
            "id": "1"
        }}"#,
            batch_number
        ))
        .send()
        .await?;

    if response.status().is_success() {
        let json = response.json::<L1BatchRangeJson>().await?;
        let l2_block_hex = json.result[0].clone();
        let without_prefix = l2_block_hex.trim_start_matches("0x");
        let l2_block = i64::from_str_radix(without_prefix, 16);

        let response_2 = client
            .post(domain)
            .header("Content-Type", "application/json")
            .body(format!(
                r#"{{
                "jsonrpc": "2.0",
                "method": "en_syncL2Block",
                "params": [{}, false],
                "id": "1"
            }}"#,
                l2_block.unwrap()
            ))
            .send()
            .await?;

        if response_2.status().is_success() {
            let json_2 = response_2.json::<JSONL2SyncRPCResponse>().await?;
            let version = json_2
                .result
                .protocol_version
                .strip_prefix("Version")
                .unwrap();

            tracing::info!("Batch {} has protocol version {}", batch_number, version);

            Ok(version.to_string())
        } else {
            Err(VerificationError::Other(
                "Failed to fetch protocol version".to_string(),
            ))
        }
    } else {
        Err(VerificationError::Other(
            "Failed to fetch protocol version".to_string(),
        ))
    }
}

// Fetches given batch information from Era RPC
pub async fn fetch_batch_commit_tx(
    batch_number: u64,
) -> Result<(String, Option<String>), VerificationError> {
    tracing::info!(
        "Fetching batch {} information from zkSync Era",
        batch_number
    );

    let domain = "https://mainnet.era.zksync.io";
    let client = reqwest::Client::new();

    let response = client
        .post(domain)
        .header("Content-Type", "application/json")
        .body(format!(
            r#"{{
            "jsonrpc": "2.0",
            "method": "zks_getL1BatchDetails",
            "params": [{}, false],
            "id": "1"
        }}"#,
            batch_number
        ))
        .send()
        .await?;

    if response.status().is_success() {
        let json = response.json::<JSONL2RPCResponse>().await?;

        Ok((json.result.commit_tx_hash, json.result.prove_tx_hash))
    } else {
        Err(VerificationError::FetchError(
            "Failed to fetch batch commit transaction".to_string(),
        ))
    }
}

pub async fn fetch_l1_commit_data(
    batch_number: u64,
    rpc_url: &str,
) -> Result<BatchL1Data, VerificationError> {
    let client = Provider::<Http>::try_from(rpc_url).expect("Failed to connect to provider");

    let contract_abi: Abi = Abi::load(&include_bytes!("abis/IZkSync.json")[..]).unwrap();
    let (function_name, fallback_fn_name) = ("commitBatchesSharedBridge", Some("commitBatches"));

    let function = contract_abi.functions_by_name(function_name).unwrap()[0].clone();
    let fallback_function =
        fallback_fn_name.map(|fn_name| contract_abi.functions_by_name(fn_name).unwrap()[0].clone());

    let previous_batch_number = batch_number - 1;
    let address =
        ethereum_types::Address::from_str("32400084c286cf3e17e7b677ea9583e60a000324").unwrap();

    let mut roots = vec![];
    let mut l1_block_number = 0;
    let mut prev_batch_commitment = H256::default();
    let mut curr_batch_commitment = H256::default();
    for b_number in [previous_batch_number, batch_number] {
        let (commit_tx, _) = fetch_batch_commit_tx(b_number).await?;

        let tx = client
            .get_transaction(TxHash::from_str(&commit_tx).unwrap())
            .await?;

        let tx = tx.unwrap();
        l1_block_number = tx.block_number.unwrap().as_u64();
        let calldata = tx.input.to_vec();

        let found_data =
            find_state_data_from_log(b_number, &function, fallback_function.clone(), &calldata)?;

        let found_data = found_data.unwrap();

        let batch_commitment = client
            .get_transaction_receipt(tx.hash)
            .await?
            .unwrap()
            .logs
            .iter()
            .find(|log| {
                log.address == address
                    && log.topics.len() == 4
                    && log.topics[0] == *BLOCK_COMMIT_EVENT_SIGNATURE
                    && log.topics[1] == H256::from_low_u64_be(b_number)
            })
            .map(|log| log.topics[3]);

        if batch_commitment.is_none() {
            return Err(VerificationError::FetchError(
                "Failed to find batch commitment".to_string(),
            ));
        }

        if b_number == previous_batch_number {
            prev_batch_commitment = batch_commitment.unwrap();
        } else {
            curr_batch_commitment = batch_commitment.unwrap();
        }

        roots.push(found_data);
    }

    assert_eq!(roots.len(), 2);

    let (previous_enumeration_counter, previous_root) = roots[0].clone();
    let (new_enumeration_counter, new_root) = roots[1].clone();

    tracing::info!(
        "Will be verifying a proof for state transition from root {} to root {}",
        format!("0x{}", hex::encode(&previous_root)),
        format!("0x{}", hex::encode(&new_root))
    );

    let base_contract: BaseContract = contract_abi.into();
    let contract_instance = base_contract.into_contract::<Provider<Http>>(address, client);
    let bootloader_code_hash = contract_instance
        .method::<_, H256>("getL2BootloaderBytecodeHash", ())
        .unwrap()
        .block(l1_block_number)
        .call()
        .await
        .unwrap();
    let default_aa_code_hash = contract_instance
        .method::<_, H256>("getL2DefaultAccountBytecodeHash", ())
        .unwrap()
        .block(l1_block_number)
        .call()
        .await
        .unwrap();

    tracing::info!(
        "Will be using bootloader code hash {} and default AA code hash {}",
        format!("0x{}", hex::encode(bootloader_code_hash.as_bytes())),
        format!("0x{}", hex::encode(default_aa_code_hash.as_bytes()))
    );
    let result = BatchL1Data {
        previous_enumeration_counter,
        previous_root,
        new_enumeration_counter,
        new_root,
        bootloader_hash: *bootloader_code_hash.as_fixed_bytes(),
        default_aa_hash: *default_aa_code_hash.as_fixed_bytes(),
        prev_batch_commitment,
        curr_batch_commitment,
    };

    Ok(result)
}

fn find_state_data_from_log(
    batch_number: u64,
    function: &Function,
    fallback_function: Option<Function>,
    calldata: &[u8],
) -> Result<Option<(u64, Vec<u8>)>, VerificationError> {
    use ethers::abi;

    if calldata.len() < 5 {
        return Err(VerificationError::FetchError(
            "Calldata is too short".to_string(),
        ));
    }

    let mut parsed_input = function.decode_input(&calldata[4..]).unwrap_or_else(|_| {
        fallback_function
            .unwrap()
            .decode_input(&calldata[4..])
            .unwrap()
    });

    let second_param = parsed_input.pop().unwrap();
    let first_param = parsed_input.pop().unwrap();

    let abi::Token::Tuple(first_param) = first_param else {
        return Err(VerificationError::FetchError(
            "Failed to parse first param".to_string(),
        ));
    };

    let abi::Token::Uint(_previous_l2_block_number) = first_param[0].clone() else {
        return Err(VerificationError::FetchError(
            "Failed to parse first param".to_string(),
        ));
    };
    // if _previous_l2_block_number.0[0] != batch_number {
    //     return Err(VerificationError::FetchError(
    //         "Batch number mismatch".to_string(),
    //     ));
    // }
    let abi::Token::Uint(previous_enumeration_index) = first_param[2].clone() else {
        return Err(VerificationError::FetchError(
            "Failed to parse second param".to_string(),
        ));
    };
    let _previous_enumeration_index = previous_enumeration_index.0[0];

    let abi::Token::Array(inner) = second_param else {
        return Err(VerificationError::FetchError(
            "Failed to parse second param".to_string(),
        ));
    };

    let mut found_params = None;

    for inner in inner.into_iter() {
        let abi::Token::Tuple(inner) = inner else {
            return Err(VerificationError::FetchError(
                "Failed to parse inner tuple".to_string(),
            ));
        };
        let abi::Token::Uint(new_l2_block_number) = inner[0].clone() else {
            return Err(VerificationError::FetchError(
                "Failed to parse new l2 block number".to_string(),
            ));
        };
        let new_l2_block_number = new_l2_block_number.0[0];
        if new_l2_block_number == batch_number {
            let abi::Token::Uint(new_enumeration_index) = inner[2].clone() else {
                return Err(VerificationError::FetchError(
                    "Failed to parse new enumeration index".to_string(),
                ));
            };
            let new_enumeration_index = new_enumeration_index.0[0];

            let abi::Token::FixedBytes(state_root) = inner[3].clone() else {
                return Err(VerificationError::FetchError(
                    "Failed to parse state root".to_string(),
                ));
            };

            assert_eq!(state_root.len(), 32);

            found_params = Some((new_enumeration_index, state_root));
        } else {
            continue;
        }
    }

    Ok(found_params)
}

pub(crate) async fn fetch_proof_from_l1(
    batch_number: u64,
    rpc_url: &str,
    protocol_version: u16,
) -> Result<(ViaZKProof, u64), VerificationError> {
    let client = Provider::<Http>::try_from(rpc_url).expect("Failed to connect to provider");

    let contract_abi: Abi = Abi::load(&include_bytes!("abis/IZkSync.json")[..]).unwrap();

    let function_name = if protocol_version < 23 {
        "proveBatches"
    } else {
        "proveBatchesSharedBridge"
    };

    let function = contract_abi.functions_by_name(function_name).unwrap()[0].clone();

    let (_, prove_tx) = fetch_batch_commit_tx(batch_number)
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)
        .unwrap();

    if prove_tx.is_none() {
        let msg = format!(
            "Proof doesn't exist for batch {}, please try again soon. Exiting...",
            batch_number,
        );
        tracing::error!("{}", msg);
        return Err(VerificationError::FetchError(msg));
    };

    let tx = client
        .get_transaction(TxHash::from_str(&prove_tx.unwrap()).unwrap())
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)
        .unwrap()
        .unwrap();

    let l1_block_number = tx.block_number.unwrap().as_u64();
    let calldata = tx.input.to_vec();

    let parsed_input = function.decode_input(&calldata[4..]).unwrap();

    let Token::Tuple(proof) = parsed_input.as_slice().last().unwrap() else {
        return Err(VerificationError::FetchError(
            "Failed to parse proof from input".to_string(),
        ));
    };

    assert_eq!(proof.len(), 2);

    let Token::Array(serialized_proof) = proof[1].clone() else {
        return Err(VerificationError::FetchError(
            "Failed to parse proof from input".to_string(),
        ));
    };

    let proof = serialized_proof
        .iter()
        .filter_map(|e| {
            if let Token::Uint(x) = e {
                Some(*x)
            } else {
                None
            }
        })
        .collect::<Vec<U256>>();

    if serialized_proof.is_empty() {
        let msg = format!("Proof doesn't exist for batch {}, exiting...", batch_number,);
        tracing::error!("{}", msg);
        return Err(VerificationError::FetchError(msg));
    }

    let x: circuit_definitions::snark_wrapper::franklin_crypto::bellman::plonk::better_better_cs::proof::Proof<Bn256, ZkSyncSnarkWrapperCircuit> = deserialize_proof(proof);

    Ok((ViaZKProof { proof: x }, l1_block_number))
}
