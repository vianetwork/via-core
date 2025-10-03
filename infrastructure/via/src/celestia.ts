import { Command } from 'commander';
import * as fs from 'fs/promises';
import * as path from 'path';
import { exec } from 'child_process';
import { updateEnvVariable } from './helpers';
import { VIA_DOCKER_COMPOSE } from './docker';

const CONTAINER_NAME = 'celestia-node';

enum DaBackend {
    CELESTIA = 'celestia',
    HTTP = 'http'
}

// Function to execute a shell command and return it as a Promise
function runCommand(command: string): Promise<string> {
    return new Promise((resolve, reject) => {
        exec(command, (error, stdout, stderr) => {
            if (error) {
                reject(`Error: ${error.message}`);
            } else if (stderr) {
                reject(`Stderr: ${stderr}`);
            } else {
                resolve(stdout.trim());
            }
        });
    });
}

const getNodeAddressCommand = `docker compose -f ${VIA_DOCKER_COMPOSE} exec ${CONTAINER_NAME} celestia state account-address | jq -r '.result'`;
const getAuthNodeCommand = `docker compose -f ${VIA_DOCKER_COMPOSE} exec ${CONTAINER_NAME} celestia light auth admin --p2p.network mocha`;
const restartCelestiaContainerCommand = `docker compose -f ${VIA_DOCKER_COMPOSE} restart ${CONTAINER_NAME}`;

async function updateEnvironment(authToken: string, filePath: string, authTokenEnv: string) {
    const envFilePath = path.join(process.env.VIA_HOME!, filePath);

    await updateEnvVariable(envFilePath, authTokenEnv, authToken);

    console.log(`Updated ${authTokenEnv} with: ${authToken}`);
}

async function fixCelestiaConfig() {
    const configFilePath = path.join(process.env.VIA_HOME!, 'volumes/celestia/config.toml');
    const configFileContent = await fs.readFile(configFilePath, 'utf-8');

    // Split the file into lines
    const lines = configFileContent.split('\n');

    // Flags to track if we are in the [RPC] section
    let inRpcSection = false;

    // Iterate over each line to find and modify the [RPC] section
    const updatedLines = lines.map((line) => {
        const trimmedLine = line.trim();

        // Check if we are entering the [RPC] section
        if (trimmedLine === '[RPC]') {
            inRpcSection = true;
            return line; // Keep the section header as is
        }

        // If we are in the [RPC] section, modify the relevant lines
        if (inRpcSection) {
            if (trimmedLine.startsWith('Address')) {
                return '  Address = "0.0.0.0"';
            }
            if (trimmedLine.startsWith('Port')) {
                return '  Port = "26658"';
            }
            if (trimmedLine.startsWith('SkipAuth')) {
                return '  SkipAuth = false';
            }

            // Check if we are leaving the [RPC] section
            if (trimmedLine.startsWith('[') && trimmedLine !== '[RPC]') {
                inRpcSection = false;
            }
        }

        // Return the line unchanged if no modifications are needed
        return line;
    });

    // Get the celestia block height where start the node
    const envFilePath = path.join(process.env.VIA_HOME!, `etc/env/l2-inits/${process.env.VIA_ENV}.init.env`);
    const envs = (await fs.readFile(envFilePath, 'utf-8')).split('\n');
    let height = '1';
    for (let i = 0; i < envs.length; i++) {
        if (envs[i].startsWith('VIA_CELESTIA_CLIENT_TRUSTED_BLOCK_HEIGHT')) {
            height = envs[i].split('=')[1];
            break;
        }
    }

    await runCommand(
        `docker compose -f ${VIA_DOCKER_COMPOSE} exec ${CONTAINER_NAME} sed -i 's/  SyncFromHeight = 0/  SyncFromHeight = ${height}/' config.toml`
    );

    // Join the updated lines back into a single string
    const updatedConfigFileContent = updatedLines.join('\n');

    // Write the updated content back to the file
    await fs.writeFile(configFilePath, updatedConfigFileContent, 'utf-8');
}

export async function viaDa(daBackend: string) {
    switch (daBackend) {
        case DaBackend.CELESTIA:
            console.log('Setup celestia');
            return viaCelestia();
        case DaBackend.HTTP:
            console.log('Setup DA proxy');
            return viaDaProxy();
    }
}

export async function viaDaProxy() {
    const auth_token = await getCelestiaAuthToken();

    await updateEnvironment(auth_token, 'via-core-ext/.env.example', 'VIA_DA_CLIENT_AUTH_TOKEN');
    await updateEnvironment(
        `http://${CONTAINER_NAME}:26658`,
        'via-core-ext/.env.example',
        'VIA_DA_CLIENT_API_NODE_URL'
    );
}

export async function getCelestiaAuthToken(): Promise<string> {
    process.chdir(`${process.env.VIA_HOME}`);
    let authToken = '';
    let nodeAddress = '';
    try {
        authToken = await runCommand(getAuthNodeCommand);
        nodeAddress = await runCommand(getNodeAddressCommand);
        console.log('Celestia Node Address:', nodeAddress);
    } catch (error) {
        console.error('Error executing command:', error);
    }
    return authToken;
}

export async function viaCelestia() {
    const auth_token = await getCelestiaAuthToken();

    await updateEnvironment(auth_token, `etc/env/target/${process.env.VIA_ENV}.env`, 'VIA_CELESTIA_CLIENT_AUTH_TOKEN');

    await fixCelestiaConfig();

    try {
        await runCommand(restartCelestiaContainerCommand);
    } catch (error) {
        console.error('Error executing command:', error);
    }
}

export const command = new Command('celestia')
    .description('VIA celestia')
    .requiredOption('--backend <backend>', 'The DA backend', DaBackend.CELESTIA)
    .action((cmd: Command) => {
        viaDa(cmd.backend);
    });
