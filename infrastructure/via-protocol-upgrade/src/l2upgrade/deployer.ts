import { spawn } from 'utils';

export async function callSystemContractDeployer(
    privateKey: string,
    l2RpcProvider: string,
    bootloader: boolean,
    defaultAA: boolean,
    systemContracts: boolean,
    file: string
) {
    const cwd = process.cwd();
    process.chdir(`${process.env.VIA_HOME}/contracts/system-contracts`);
    let argsString = '';
    if (bootloader) {
        argsString += ' --bootloader';
    }
    if (defaultAA) {
        argsString += ' --default-aa';
    }
    if (systemContracts) {
        argsString += ' --system-contracts';
    }
    if (file) {
        argsString += ` --file ${file}`;
    }
    if (l2RpcProvider) {
        argsString += ` --l2Rpc ${l2RpcProvider}`;
    }
    if (privateKey) {
        argsString += ` --private-key ${privateKey}`;
    }
    await spawn(`yarn deploy-preimages ${argsString}`);
    process.chdir(cwd);
}
