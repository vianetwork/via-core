import * as fs from 'fs/promises';
import * as dotenv from 'dotenv';
import * as bitcoin from 'bitcoinjs-lib';
import * as bip39 from 'bip39';
import * as ecc from 'tiny-secp256k1';
import { BIP32Factory } from 'bip32';
import { readFileSync, writeFileSync } from 'fs';

export async function updateEnvVariable(envFilePath: string, variableName: string, newValue: string) {
    const envFileContent = await fs.readFile(envFilePath, 'utf-8');
    const envConfig = dotenv.parse(envFileContent);

    envConfig[variableName] = newValue;

    let newEnvContent = '';
    for (const key in envConfig) {
        newEnvContent += `${key}=${envConfig[key]}\n`;
    }

    await fs.writeFile(envFilePath, newEnvContent, 'utf-8');
}

const bip32 = BIP32Factory(ecc);

interface Wallet {
    mnemonic: string;
    privateKey: string;
    address: string;
    publicKey: string,
    network: string;
}

export function getNetwork(network: string) {
    switch (network) {
        case 'testnet':
            return bitcoin.networks.testnet;
        case 'bitcoin':
            return bitcoin.networks.bitcoin;
        default:
            return bitcoin.networks.regtest;
    }
}

export async function generateBitcoinWallet(network: string = 'regtest'): Promise<Wallet | null> {
    try {
        const btcNetwork = getNetwork(network);
        // Generate mnemonic
        const mnemonic: string = bip39.generateMnemonic();

        // Create seed from mnemonic
        const seed = await bip39.mnemonicToSeed(mnemonic);

        // Create root key
        const root = bip32.fromSeed(Buffer.from(seed), btcNetwork);

        // Derive wallet (BIP84 path for native SegWit)
        const path: string = "m/84'/0'/0'/0/0";
        const child = root.derivePath(path);

        // Generate address
        const { address } = bitcoin.payments.p2wpkh({
            pubkey: Buffer.from(child.publicKey),
            network: btcNetwork
        });

        if (!address) {
            throw new Error('Failed to generate address');
        }

        return {
            mnemonic,
            privateKey: child.toWIF(),
            address,
            publicKey: Buffer.isBuffer(child.publicKey)
                ? child.publicKey.toString('hex')
                : Buffer.from(child.publicKey).toString('hex'),
            network: network
        };
    } catch (error) {
        console.error('Error generating wallet:', (error as Error).message);
        return null;
    }
}

export const readJsonFile = <T>(filePath: string): T => {
    try {
        const fileContent = readFileSync(filePath, 'utf-8');
        return JSON.parse(fileContent) as T;
    } catch (error) {
        console.error(`Error reading or parsing file at ${filePath}:`, error);
        throw error;
    }
};

export const writeJsonFile = (filePath: string, data: object): void => {
    try {
        writeFileSync(filePath, JSON.stringify(data, null, 2));
        console.log(`\nData successfully saved to ${filePath}`);
    } catch (error) {
        console.error(`Error writing file to ${filePath}:`, error);
        throw error;
    }
};