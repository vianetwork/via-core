import * as utils from 'utils';
import { Command } from 'commander';
import * as fs from 'fs/promises';
import * as path from 'path';
import * as dotenv from 'dotenv';

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

export async function updateBootstrapTxidsEnv() {
    const txidsFilePath = path.join(process.env.VIA_HOME!, 'txids.via');

    const txidsContent = await fs.readFile(txidsFilePath, 'utf8');
    const txidsLines = txidsContent.split('\n');
    txidsLines.pop(); // Remove last empty line

    const newTxids = txidsLines.join(',');

    const envFilePath = path.join(process.env.VIA_HOME!, `etc/env/target/${process.env.VIA_ENV}.env`);

    console.log(`Updating file ${envFilePath}`);

    await updateEnvVariable(envFilePath, 'VIA_BTC_WATCH_BOOTSTRAP_TXIDS', newTxids);

    console.log(`Updated VIA_BTC_WATCH_BOOTSTRAP_TXIDS with: ${newTxids}`);

    try {
        // await fs.unlink(txidsFilePath);
        console.log(`NOT Deleted txids.via file.`);
    } catch (error) {
        console.error(`Error deleting txids.via file`);
    }
}

export async function via_bootstrap(network: string, rpcUrl: string, rpcUsername: string, rpcPassword: string) {
    process.chdir(`${process.env.VIA_HOME}`);
    await utils.spawn(`cargo run --example bootstrap -- ${network} ${rpcUrl} ${rpcUsername} ${rpcPassword}`);

    await updateBootstrapTxidsEnv();
}

export const command = new Command('bootstrap')
    .description('VIA bootstrap')
    .option('--network <network>', 'network', DEFAULT_NETWORK)
    .option('--rpc-url <rpcUrl>', 'RPC URL', DEFAULT_RPC_URL)
    .option('--rpc-username <rpcUsername>', 'RPC username', DEFAULT_RPC_USERNAME)
    .option('--rpc-password <rpcPassword>', 'RPC password', DEFAULT_RPC_PASSWORD)
    .action((cmd: Command) => via_bootstrap(cmd.network, cmd.rpcUrl, cmd.rpcUsername, cmd.rpcPassword));
