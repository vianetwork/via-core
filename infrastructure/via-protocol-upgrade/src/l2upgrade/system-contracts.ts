import fs from 'fs';
import { Command } from 'commander';
import { getL2UpgradeFileName, getUpgradePath } from '../utils';
import { callSystemContractDeployer } from './deployer';

async function publishAndMergeFiles(
    privateKey: string,
    l2RpcProvider: string,
    bootloader: boolean,
    defaultAA: boolean,
    systemContracts: boolean,
    environment: string,
    newProtocolVersion: string
) {
    console.log('Publishing bytecodes for system contracts');
    validateProtcolVersion(newProtocolVersion);

    const upgradePath = getUpgradePath(environment);
    const tmpUpgradeFile = upgradePath + '/tmp.json';
    await callSystemContractDeployer(privateKey, l2RpcProvider, bootloader, defaultAA, systemContracts, tmpUpgradeFile);
    const mainUpgradeFile = getL2UpgradeFileName(environment);
    let tmpUpgradeData = JSON.parse(fs.readFileSync(tmpUpgradeFile, 'utf8'));

    if (!fs.existsSync(mainUpgradeFile)) {
        fs.writeFileSync(mainUpgradeFile, JSON.stringify(tmpUpgradeData, null, 2));
        fs.unlinkSync(tmpUpgradeFile);
        return;
    }

    let mainUpgradeData = JSON.parse(fs.readFileSync(mainUpgradeFile, 'utf8'));
    if (bootloader !== undefined) {
        mainUpgradeData.bootloader = tmpUpgradeData.bootloader;
    }
    if (defaultAA !== undefined) {
        mainUpgradeData.defaultAA = tmpUpgradeData.defaultAA;
    }
    if (systemContracts) {
        mainUpgradeData.systemContracts = tmpUpgradeData.systemContracts;
    }

    mainUpgradeData.version = newProtocolVersion;
    fs.writeFileSync(mainUpgradeFile, JSON.stringify(mainUpgradeData, null, 2));
    fs.unlinkSync(tmpUpgradeFile);
    console.log('All system contracts published');
}

function isNumericString(str: string) {
    return str.trim() !== '' && Number.isFinite(Number(str));
}

function validateProtcolVersion(newProtocolVersion: string) {
    const protocolSemanticVersion = newProtocolVersion.split('.');
    if (protocolSemanticVersion.length != 3) {
        throw new Error('Invalid protocol version, should be 0.X.X');
    }

    for (let i = 0; i < protocolSemanticVersion.length; i++) {
        if (!isNumericString(protocolSemanticVersion[i])) {
            throw new Error('Invalid protocol string, should be numbers 0.X.X');
        }
    }
}
export const command = new Command('system-contracts').description('publish system contracts');

// Example cmd:
//  yarn start system-contracts publish --private-key 0x7726827caac94a7f9e1b160f7ea819f172f7b6f9d2a97f992c38edeab82d4110 \
//  --l2rpc http://0.0.0.0:3050 \
//  --environment devnet-2 \
//  --new-protocol-version 26 \
//  --bootloader --default-aa --system-contracts
command
    .command('publish')
    .description('Publish contracts one by one')
    .option('--private-key <private-key>')
    .option('--l2rpc <l2Rpc>')
    .option('--environment <environment>')
    .option('--new-protocol-version <newProtocolVersion>')
    .option('--bootloader')
    .option('--default-aa')
    .option('--system-contracts')
    .action(async (cmd) => {
        await publishAndMergeFiles(
            cmd.privateKey,
            cmd.l2rpc,
            cmd.bootloader,
            cmd.defaultAa,
            cmd.systemContracts,
            cmd.environment,
            cmd.newProtocolVersion
        );
    });
