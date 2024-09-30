import * as utils from 'utils';
import { Command } from 'commander';
import * as fs from 'fs/promises';
import * as path from 'path';
import * as dotenv from 'dotenv';

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

async function updateBootstrapTxidsEnv() {
    const txidsFilePath = path.join(process.env.VIA_HOME!, 'txids.via');

    const txidsContent = await fs.readFile(txidsFilePath, 'utf8');
    const txidsLines = txidsContent.split('\n').slice(0, 3);

    const newTxids = txidsLines.join(',');

    const envFilePath = path.join(process.env.VIA_HOME!, `etc/env/target/${process.env.VIA_ENV}.env`);

    await updateEnvVariable(envFilePath, 'VIA_BTC_WATCH_BOOTSTRAP_TXIDS', newTxids);

    console.log(`Updated VIA_BTC_WATCH_BOOTSTRAP_TXIDS with: ${newTxids}`);

    try {
        await fs.unlink(txidsFilePath);
        console.log(`Deleted txids.via file.`);
    } catch (error) {
        console.error(`Error deleting txids.via file`);
    }
}

export async function via_bootstrap() {
    process.chdir(`${process.env.VIA_HOME}`);
    await utils.spawn(`cargo run --example bootstrap`);

    await updateBootstrapTxidsEnv();
}

export const command = new Command('bootstrap').description('VIA bootstrap').action(async () => {
    await via_bootstrap();
});
