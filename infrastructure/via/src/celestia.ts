import * as utils from 'utils';
import { Command } from 'commander';
import * as fs from 'fs/promises';
import * as path from 'path';
import * as dotenv from 'dotenv';
import { exec } from 'child_process';

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
    return data.token;
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

    try {
        await get_celestia_faucet_token(node_address);
        await get_celestia_faucet_token(node_address);
        console.log('Request Sent to Faucet');
        console.log(`Check your balance at https://arabica.celenium.io/address/${node_address}?tab=transactions`);
    } catch (error) {
        console.error('Error getting faucet token:', error);
    }
}

export const command = new Command('celestia').description('VIA celestia').action(via_celestia);
