import * as utils from 'utils';
import { Command } from 'commander';
import * as fs from 'fs/promises';
import * as path from 'path';
import * as dotenv from 'dotenv';
import { parse } from 'yaml';
import { readFileSync } from 'fs';

const DEFAULT_NETWORK = 'regtest';
const DEFAULT_RPC_URL = 'http://0.0.0.0:18443';
const DEFAULT_RPC_USERNAME = 'rpcuser';
const DEFAULT_RPC_PASSWORD = 'rpcpassword';

async function updateEnvVariable(envFilePath: string, variableName: string, newValue: string) {
    const envFileContent = await fs.readFile(envFilePath, 'utf-8');
    const envConfig = dotenv.parse(envFileContent);

    envConfig[variableName] = newValue;

    let newEnvContent = '';
    for (const key in envConfig) {
        newEnvContent += `${key}=${envConfig[key]}\n`;
    }

    await fs.writeFile(envFilePath, newEnvContent, 'utf-8');
}

export async function updateBootstrapTxidsEnv(network: string) {
    let genesisTxIds = process.env.VIA_GENESIS_BOOTSTRAP_TXIDS;

    if (!genesisTxIds || genesisTxIds === '""') {
        const genesisDir = path.join(process.env.VIA_HOME!, `etc/env/via/genesis/${network}`);
        const files = await fs.readdir(genesisDir);

        const txids = [];
        // Process first the System inscriptions
        const data = JSON.parse(await fs.readFile(path.join(genesisDir, 'SystemBootstrapping.json'), 'utf-8'));
        if (data.tx_type != 'SystemBootstrapping') {
            throw Error('Invalid System Bootstrapping');
        }
        txids.push(data.system_tx_id);
        txids.push(data.propose_sequencer_tx_id);

        // Process the Attestation
        for (let i = 0; i < files.length; i++) {
            const data = JSON.parse(await fs.readFile(path.join(genesisDir, files[i]), 'utf-8'));
            if (data.tx_type == 'Attest') {
                txids.push(data.tx_id);
            }
        }
        genesisTxIds = txids.join(',');
    }

    const envFilePath = path.join(process.env.VIA_HOME!, `etc/env/target/${process.env.VIA_ENV}.env`);
    console.log(`Updating file ${envFilePath}`);

    await updateEnvVariable(envFilePath, 'VIA_GENESIS_BOOTSTRAP_TXIDS', genesisTxIds);

    console.log(`Updated VIA_GENESIS_BOOTSTRAP_TXIDS with: ${genesisTxIds}`);
}

export async function systemBootstrapping(
    network: string,
    rpcUrl: string,
    rpcUsername: string,
    rpcPassword: string,
    privateKey: string,
    startBlock: string,
    verifiersPubKeys: string,
    bridgeWalletPath: string,
    governanceAddress: string,
    sequencerAddress: string
) {
    process.chdir(`${process.env.VIA_HOME}`);

    const genesisPath = `etc/env/file_based/genesis.yaml`;
    const file = await fs.readFile(genesisPath, 'utf-8');
    const genesisData = parse(file);

    const bridgeWallet = JSON.parse(readFileSync(bridgeWalletPath, "utf-8"))

    const merkleRoot = bridgeWallet['merkle_root']
    const bridgeAddress = bridgeWallet['taproot_address']
    const bridgeVerifiersPubKeys = bridgeWallet['public_keys'].join(',')


    const default_aa_hash = genesisData['default_aa_hash'];
    const bootloader_hash = genesisData['bootloader_hash'];
    const snark_wrapper_vk_hash = genesisData['prover']['snark_wrapper_vk_hash'];
    const evm_emulator_hash = '0x0000000000000000000000000000000000000000000000000000000000000000';
    const protocolVersion = genesisData['genesis_protocol_semantic_version']

    let cmd = `cargo run --example bootstrap ${network} ${rpcUrl} ${rpcUsername} ${rpcPassword} SystemBootstrapping ${privateKey} `;
    cmd += `${startBlock} ${protocolVersion} ${bootloader_hash} ${default_aa_hash} ${snark_wrapper_vk_hash} ${evm_emulator_hash} `;
    cmd += `${governanceAddress} ${sequencerAddress} ${bridgeAddress} ${merkleRoot} ${bridgeVerifiersPubKeys} ${verifiersPubKeys}`;

    await utils.spawn(cmd);
}

export const command = new Command('bootstrap');

command
    .command('system-bootstrapping')
    .description('Create a system bootstrapping inscription')
    .option('--network <network>', 'network', DEFAULT_NETWORK)
    .option('--rpc-url <rpcUrl>', 'RPC URL', DEFAULT_RPC_URL)
    .option('--rpc-username <rpcUsername>', 'RPC username', DEFAULT_RPC_USERNAME)
    .option('--rpc-password <rpcPassword>', 'RPC password', DEFAULT_RPC_PASSWORD)
    .requiredOption('--start-block <startBlock>', 'Start block')
    .requiredOption('--private-key <privateKey>', 'The inscriber private key')
    .requiredOption('--bridge-wallet-path <bridgeWalletPath>', 'The musig2 bridge address file path')
    .requiredOption('--verifiers-pub-keys <verifiersPubKeys>', 'verifiers public keys')
    .requiredOption('--governance-address <governanceAddress>', 'The governance address')
    .requiredOption('--sequencer-address <sequencerAddress>', 'The sequencer address')
    .action((cmd: Command) =>
        systemBootstrapping(
            cmd.network,
            cmd.rpcUrl,
            cmd.rpcUsername,
            cmd.rpcPassword,
            cmd.privateKey,
            cmd.startBlock,
            cmd.verifiersPubKeys,
            cmd.bridgeWalletPath,
            cmd.governanceAddress,
            cmd.sequencerAddress
        )
    );

command
    .command('update-bootstrap-tx')
    .description('Update the bootstrap envs')
    .option('--network <network>', 'network', DEFAULT_NETWORK)
    .action((cmd: Command) => updateBootstrapTxidsEnv(cmd.network));
