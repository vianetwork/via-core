import * as utils from 'utils';
import { Command } from 'commander';
import * as fs from 'fs/promises';
import * as path from 'path';
import * as dotenv from 'dotenv';
import { exec } from 'child_process';
import { updateEnvVariable } from './helpers';

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

const get_node_address_command =
    "docker exec $(docker ps -q -f name=celestia-node) celestia state account-address | jq -r '.result'";
const get_auth_node_command =
    'docker exec $(docker ps -q -f name=celestia-node) celestia light auth admin --p2p.network arabica';
const restart_celestia_container_command = 'docker restart celestia-node';

async function updateEnvironment(auth_token: string) {
    const envFilePath = path.join(process.env.VIA_HOME!, `etc/env/target/${process.env.VIA_ENV}.env`);

    await updateEnvVariable(envFilePath, 'VIA_CELESTIA_CLIENT_AUTH_TOKEN', auth_token);

    console.log(`Updated VIA_CELESTIA_CLIENT_AUTH_TOKEN with: ${auth_token}`);
}

async function get_celestia_faucet_token(node_address: string) {
    const response = await fetch('https://faucet.celestia-arabica-11.com/api/v1/faucet/give_me', {
        headers: {
            accept: '*/*',
            'accept-language': 'en-US,en;q=0.9',
            'content-type': 'application/json',
            priority: 'u=1, i',
            'sec-ch-ua': '"Google Chrome";v="129", "Not=A?Brand";v="8", "Chromium";v="129"',
            'sec-ch-ua-mobile': '?0',
            'sec-ch-ua-platform': '"macOS"',
            'sec-fetch-dest': 'empty',
            'sec-fetch-mode': 'cors',
            'sec-fetch-site': 'same-origin',
            Referer: 'https://faucet.celestia-arabica-11.com/',
            'Referrer-Policy': 'strict-origin-when-cross-origin'
        },
        body: JSON.stringify({
            address: node_address,
            chainId: 'arabica-11'
        }),
        method: 'POST'
    });

    const data = await response.json();
    console.log('Faucet Response:', data);
    return data.token;
}

async function fix_celestia_config() {
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

    await runCommand(`docker exec celestia-node sed -i 's/  SampleFrom = 1/  SampleFrom = ${height}/' config.toml`);

    // Join the updated lines back into a single string
    const updatedConfigFileContent = updatedLines.join('\n');

    // Write the updated content back to the file
    await fs.writeFile(configFilePath, updatedConfigFileContent, 'utf-8');
}

export async function via_celestia() {
    process.chdir(`${process.env.VIA_HOME}`);
    let auth_token = '';
    let node_address = '';
    try {
        auth_token = await runCommand(get_auth_node_command);
        node_address = await runCommand(get_node_address_command);
        console.log('Celestia Node Address:', node_address);
    } catch (error) {
        console.error('Error executing command:', error);
    }

    await updateEnvironment(auth_token);

    await fix_celestia_config();

    try {
        await runCommand(restart_celestia_container_command);
    } catch (error) {
        console.error('Error executing command:', error);
    }

    try {
        console.log('Request Sent to Faucet');
        await get_celestia_faucet_token(node_address);
        await get_celestia_faucet_token(node_address);
        console.log(`Check your balance at https://arabica.celenium.io/address/${node_address}?tab=transactions`);
    } catch (error) {
        console.error('Error getting faucet token:', error);
    }
}

export const command = new Command('celestia').description('VIA celestia').action(via_celestia);
