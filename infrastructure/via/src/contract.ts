import { Command } from 'commander';
import * as utils from 'utils';
import * as env from './env';
import fs from 'fs';
import { Wallet } from 'ethers';
import path from 'path';

export async function build(): Promise<void> {
    await utils.spawn('yarn l2-contracts build');
}


export function updateContractsEnv(initEnv: string, deployLog: String, envVars: Array<string>): string {
    let updatedContracts = '';
    for (const envVar of envVars) {
        const pattern = new RegExp(`${envVar}=.*`, 'g');
        const matches = deployLog.match(pattern);
        if (matches !== null) {
            const varContents = matches[0];
            env.modify(envVar, varContents, initEnv, false);
            updatedContracts += `${varContents}\n`;
        }
    }
    env.reload();
    return updatedContracts;
}

export async function deployL2(args: any[] = [], includePaymaster?: boolean): Promise<void> {
    await utils.confirmAction();

    const isLocalSetup = process.env.VIA_LOCAL_SETUP;

    // Skip compilation for local setup, since we already copied artifacts into the container.
    if (!isLocalSetup) {
        await utils.spawn(`yarn l2-contracts build`);
    }

    await utils.spawn(`yarn l2-contracts deploy-shared-bridge-on-l2 ${args.join(' ')} | tee deployL2.log`);

    if (includePaymaster) {
        await utils.spawn(`yarn l2-contracts deploy-testnet-paymaster ${args.join(' ')} | tee -a deployL2.log`);
    }

    await utils.spawn(`yarn l2-contracts deploy-force-deploy-upgrader ${args.join(' ')} | tee -a deployL2.log`);

    let l2DeployLog = fs.readFileSync('deployL2.log').toString();
    const l2DeploymentEnvVars = [
        'CONTRACTS_L2_SHARED_BRIDGE_ADDR',
        'CONTRACTS_L2_TESTNET_PAYMASTER_ADDR',
        'CONTRACTS_L2_WETH_TOKEN_IMPL_ADDR',
        'CONTRACTS_L2_WETH_TOKEN_PROXY_ADDR',
        'CONTRACTS_L2_DEFAULT_UPGRADE_ADDR'
    ];
    updateContractsEnv(`etc/env/l2-inits/${process.env.VIA_ENV!}.init.env`, l2DeployLog, l2DeploymentEnvVars);
}

export const command = new Command('contract').description('contract management');

command.command('build').description('build contracts').action(build);

command
    .command('deploy-l2 [deploy-opts...]')
    .allowUnknownOption(true)
    .description('deploy l2 contracts')
    .action(deployL2);
