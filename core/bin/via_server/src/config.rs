use anyhow::Context as _;
use zksync_config::{
    configs::{
        api::MerkleTreeApiConfig,
        chain::{CircuitBreakerConfig, MempoolConfig, OperationsManagerConfig, StateKeeperConfig},
        consensus::ConsensusSecrets,
        fri_prover_group::FriProverGroupConfig,
        house_keeper::HouseKeeperConfig,
        BasicWitnessInputProducerConfig, FriProofCompressorConfig, FriProverConfig,
        FriWitnessGeneratorConfig, ObservabilityConfig, PrometheusConfig, ProofDataHandlerConfig,
        ProtectiveReadsWriterConfig,
    },
    ApiConfig, ContractVerifierConfig, DADispatcherConfig, DBConfig, EthConfig, GasAdjusterConfig,
    ObjectStoreConfig, PostgresConfig, ViaBtcSenderConfig, ViaBtcWatchConfig, ViaCelestiaConfig,
};
use zksync_core_leftovers::temp_config_store::{decode_yaml_repr, TempConfigStore};
use zksync_env_config::FromEnv;
use zksync_protobuf_config::proto;

pub(crate) fn read_consensus_secrets() -> anyhow::Result<Option<ConsensusSecrets>> {
    // Read public config.
    let Ok(path) = std::env::var("CONSENSUS_SECRETS_PATH") else {
        return Ok(None);
    };
    let secrets = std::fs::read_to_string(&path).context(path)?;
    Ok(Some(
        decode_yaml_repr::<proto::secrets::ConsensusSecrets>(&secrets)
            .context("failed decoding YAML")?,
    ))
}
//
// pub(crate) fn read_consensus_config() -> anyhow::Result<Option<ConsensusConfig>> {
//     // Read public config.
//     let Ok(path) = std::env::var("CONSENSUS_CONFIG_PATH") else {
//         return Ok(None);
//     };
//     let cfg = std::fs::read_to_string(&path).context(path)?;
//     Ok(Some(
//         decode_yaml_repr::<proto::consensus::Config>(&cfg).context("failed decoding YAML")?,
//     ))
// }

pub(crate) fn load_env_config() -> anyhow::Result<TempConfigStore> {
    Ok(TempConfigStore {
        postgres_config: PostgresConfig::from_env().ok(),
        health_check_config: None,
        merkle_tree_api_config: MerkleTreeApiConfig::from_env().ok(),
        web3_json_rpc_config: None,
        circuit_breaker_config: CircuitBreakerConfig::from_env().ok(),
        mempool_config: MempoolConfig::from_env().ok(),
        network_config: None,
        contract_verifier: ContractVerifierConfig::from_env().ok(),
        operations_manager_config: OperationsManagerConfig::from_env().ok(),
        state_keeper_config: StateKeeperConfig::from_env().ok(),
        house_keeper_config: HouseKeeperConfig::from_env().ok(),
        fri_proof_compressor_config: FriProofCompressorConfig::from_env().ok(),
        fri_prover_config: FriProverConfig::from_env().ok(),
        fri_prover_group_config: FriProverGroupConfig::from_env().ok(),
        fri_prover_gateway_config: None,
        fri_witness_vector_generator: None,
        fri_witness_generator_config: FriWitnessGeneratorConfig::from_env().ok(),
        prometheus_config: PrometheusConfig::from_env().ok(),
        proof_data_handler_config: ProofDataHandlerConfig::from_env().ok(),
        api_config: ApiConfig::from_env().ok(),
        db_config: DBConfig::from_env().ok(),
        eth_sender_config: EthConfig::from_env().ok(),
        eth_watch_config: None,
        gas_adjuster_config: GasAdjusterConfig::from_env().ok(),
        observability: ObservabilityConfig::from_env().ok(),
        snapshot_creator: None,
        da_dispatcher_config: DADispatcherConfig::from_env().ok(),
        protective_reads_writer_config: ProtectiveReadsWriterConfig::from_env().ok(),
        basic_witness_input_producer_config: BasicWitnessInputProducerConfig::from_env().ok(),
        core_object_store: ObjectStoreConfig::from_env().ok(),
        base_token_adjuster_config: None,
        commitment_generator: None,
        pruning: None,
        snapshot_recovery: None,
        external_price_api_client_config: None,
        external_proof_integration_api_config: None,
        experimental_vm_config: None,
        prover_job_monitor_config: None,
    })
}

// TODO: temporary solution, should be removed after the config is refactored
pub(crate) fn via_load_env_config(
) -> anyhow::Result<(ViaBtcWatchConfig, ViaBtcSenderConfig, ViaCelestiaConfig)> {
    let btc_watch_config =
        ViaBtcWatchConfig::from_env().context("Failed to load BTC watch config")?;
    let btc_sender_config =
        ViaBtcSenderConfig::from_env().context("Failed to load BTC sender config")?;
    let celestia_config =
        ViaCelestiaConfig::from_env().context("Failed to load celestia config")?;

    Ok((btc_watch_config, btc_sender_config, celestia_config))
}
