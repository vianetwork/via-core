import * as utils from 'utils';
import { Command } from 'commander';
import { getL2UpgradeFileName } from '../utils';
import fs from 'fs';

const DEFAULT_NETWORK = 'regtest';
const DEFAULT_RPC_URL = 'http://0.0.0.0:18443';
const DEFAULT_RPC_USERNAME = 'rpcuser';
const DEFAULT_RPC_PASSWORD = 'rpcpassword';

// Create a Bitcoin protocol upgrade inscription. Only the governor can create this inscription.
export async function createUpgradeInscription(
    environment: string,
    network: string,
    rpcUrl: string,
    rpcUsername: string,
    rpcPassword: string,
    privateKey: string
) {
    const l2upgradeFileName = getL2UpgradeFileName(environment);
    if (!fs.existsSync(l2upgradeFileName)) {
        throw new Error(`No l2 upgrade file found at ${l2upgradeFileName}`);
    }

    let l2upgradeData = JSON.parse(fs.readFileSync(l2upgradeFileName, 'utf8'));

    const version = l2upgradeData['version'];
    const bootloader = l2upgradeData['bootloader']['bytecodeHashes'][0];
    const defaultAA = l2upgradeData['defaultAA']['bytecodeHashes'][0];
    const recursionSchedulerLevelVkHash = l2upgradeData['recursionSchedulerLevelVkHash'];

    const systemContractsAddresses = [];
    const systemContractsHashes = [];

    for (let i = 0; i < l2upgradeData['systemContracts'].length; i++) {
        const contract = l2upgradeData['systemContracts'][i];
        systemContractsAddresses.push(contract.address);
        systemContractsHashes.push(contract.bytecodeHashes[0]);
    }

    process.chdir(`${process.env.VIA_HOME}`);
    await utils.spawn(
        `cargo run --example upgrade_system_contracts -- ${[
            network,
            rpcUrl,
            rpcUsername,
            rpcPassword,
            privateKey,
            version,
            bootloader,
            defaultAA,
            systemContractsAddresses.join(','),
            systemContractsHashes.join(','),
            recursionSchedulerLevelVkHash
        ].join(' ')}`
    );
}

export const command = new Command('l2-transaction').description('publish system contracts');

// Example cmd:
// yarn start l2-transaction upgrade-system-contracts --environment devnet-2 --private-key cVZduZu265sWeAqFYygoDEE1FZ7wV9rpW5qdqjRkUehjaUMWLT1R
command
    .command('upgrade-system-contracts')
    .option('--environment <environment>')
    .option('--network <network>', 'network', DEFAULT_NETWORK)
    .option('--rpc-url <rpcUrl>', 'RPC URL', DEFAULT_RPC_URL)
    .option('--rpc-username <rpcUsername>', 'RPC username', DEFAULT_RPC_USERNAME)
    .option('--rpc-password <rpcPassword>', 'RPC password', DEFAULT_RPC_PASSWORD)
    .option('--private-key <privateKey>', 'The gov private key')
    .action(async (cmd) => {
        await createUpgradeInscription(
            cmd.environment,
            cmd.network,
            cmd.rpcUrl,
            cmd.rpcUsername,
            cmd.rpcPassword,
            cmd.privateKey
        );
    });
