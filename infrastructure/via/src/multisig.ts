import { Command } from 'commander';
import * as bitcoin from 'bitcoinjs-lib';
import * as ecc from 'tiny-secp256k1';
import { ECPairFactory, ECPairInterface } from 'ecpair';
import { generateBitcoinWallet, getNetwork, readJsonFile, writeJsonFile } from './helpers';
import { Psbt } from 'bitcoinjs-lib';
import axios from 'axios';
import { DEFAULT_NETWORK } from "./constants"

// Initialize the elliptic curve library
bitcoin.initEccLib(ecc);
const ECPair = ECPairFactory(ecc);

// Define a type for our UTXO input for clarity and type safety.
interface UtxoInput {
    txid: string;
    vout: number;
    amountSatoshis: number;
}

const createBitcoinWallet = async (networkStr: string = "regtest") => {
    const wallet = await generateBitcoinWallet(networkStr);

    console.log("Wallet: ", wallet);
}

// Initialise ECC (required since bitcoinjs‑lib v6)
const compute_multisig_address = (pubkeys: Array<string>, minimumSigners: number, outDir: string, networkStr: string = "regtest") => {
    bitcoin.initEccLib(ecc);

    const network = getNetwork(networkStr);
    const pubkeyBuffers = pubkeys.map(hex => Buffer.from(hex, 'hex'));

    const p2ms = bitcoin.payments.p2ms({ m: minimumSigners, pubkeys: pubkeyBuffers, network });
    const p2wsh = bitcoin.payments.p2wsh({ redeem: p2ms, network });

    const multisig = {
        address: p2wsh.address,
        witnessScript: p2wsh.redeem?.output?.toString('hex'),
        outputScript: p2wsh.output?.toString('hex'),
    }
    console.log("Multisig wallet", multisig)

    writeJsonFile(outDir, { multisig });
}

const createUnsignedUpgradeTransaction = (
    utxoInput: UtxoInput,
    upgradeTxId: string,
    fee: number,
    outDir: string,
    networkStr: string = "regtest",
) => {
    const dataFile: any = readJsonFile(outDir);
    const network = getNetwork(networkStr);

    const psbt = new Psbt({ network });

    // 1. Add the multisig input to spend
    psbt.addInput({
        hash: Buffer.from(utxoInput.txid, 'hex').reverse(),
        index: utxoInput.vout,
        witnessUtxo: {
            script: Buffer.from(dataFile.multisig.outputScript, 'hex'),
            value: utxoInput.amountSatoshis,
        },
        witnessScript: Buffer.from(dataFile.multisig.witnessScript, 'hex'),
    });

    // 2. Create and add the OP_RETURN output
    const upgradeTxIdBuffer = Buffer.from(upgradeTxId, 'hex').reverse();
    const prefixBuffer = Buffer.from("VIA_PROTOCOL:UPGRADE", 'utf8');
    const embed = bitcoin.payments.embed({ data: [prefixBuffer, upgradeTxIdBuffer] });
    if (!embed.output) throw new Error("Could not create OP_RETURN script.");

    psbt.addOutput({
        script: embed.output,
        value: 0,
    });

    // 3. Create and add the change output
    const changeValue = utxoInput.amountSatoshis - fee;
    if (changeValue < 0) {
        throw new Error("Funding amount is less than the fee.");
    }

    psbt.addOutput({
        address: dataFile.multisig.address,
        value: changeValue,
    });

    writeJsonFile(outDir, { ...dataFile, tx: psbt.toBase64() })

    console.log("Successfully created an unsigned PSBT for the upgrade transaction.");
};

/**
 * Signs a PSBT with a given private key.
 * This function correctly creates a Signer object that is compatible with bitcoinjs-lib's types.
 */
const signPsbt = (
    wifPrivateKey: string,
    outDir: string,
    networkStr: string,
) => {
    const dataFile: any = readJsonFile(outDir);
    const network = getNetwork(networkStr);

    const keyPair: ECPairInterface = ECPair.fromWIF(wifPrivateKey, network);
    const psbt = bitcoin.Psbt.fromBase64(dataFile.tx, { network });

    const signer: bitcoin.Signer = {
        publicKey: Buffer.from(keyPair.publicKey), // Convert Uint8Array to Buffer
        sign: (hash: Buffer): Buffer => {
            // Use the underlying keyPair to perform the actual signing
            return Buffer.from(keyPair.sign(hash));
        },
    };

    psbt.signInput(0, signer);

    const validationFunction = (pubkey: Buffer, msghash: Buffer, signature: Buffer): boolean =>
        ecc.verify(msghash, pubkey, signature);

    if (!psbt.validateSignaturesOfInput(0, validationFunction)) {
        throw new Error("Signature validation failed for the new signature.");
    }

    writeJsonFile(outDir, { ...dataFile, tx: psbt.toBase64() })
};


/**
 * Finalizes a PSBT and extracts the full transaction hex.
 */
const finalizeAndExtractTx = (outDir: string, networkStr: string = "regtest") => {
    const dataFile: any = readJsonFile(outDir);
    const network = getNetwork(networkStr);

    const psbt = bitcoin.Psbt.fromBase64(dataFile.tx, { network });
    psbt.finalizeAllInputs();

    const finalTx = psbt.extractTransaction();

    writeJsonFile(outDir, { ...dataFile, txHex: finalTx.toHex() })
    console.log(finalTx.toHex());
};

/**
 * Broadcasts a raw transaction hex to the Bitcoin node via JSON-RPC with authentication.
 */
const broadcastTransaction = async (
    outDir: string,
    rpc_url: string,
    rpc_user: string,
    rpc_password: string
) => {
    try {
        const dataFile: any = readJsonFile(outDir);
        console.log()

        const auth = Buffer.from(`${rpc_user}:${rpc_password}`).toString('base64');

        const response = await axios.post(
            rpc_url,
            {
                jsonrpc: '1.0',
                id: 'broadcast',
                method: 'sendrawtransaction',
                params: [dataFile.txHex],
            },
            {
                headers: {
                    'Content-Type': 'application/json',
                    'Authorization': `Basic ${auth}`,
                },
            }
        );

        if (response.data.error) {
            throw new Error(`RPC Error: ${response.data.error.message}`);
        }

        writeJsonFile(outDir, { ...dataFile, txid: response.data.result })

        console.log("Txid:", response.data.result)
    } catch (error) {
        if (axios.isAxiosError(error) && error.response) {
            console.error('RPC Error:', error.response.data);
            throw new Error(`Failed to broadcast transaction: ${error.response.data.error?.message || 'Unknown error'}`);
        }
        throw error;
    }
};

export const command = new Command('multisig').description('Multisig helper');

command
    .command('create-wallet')
    .description('Create a bitcoin wallet')
    .option('--network <network>', 'network', DEFAULT_NETWORK)
    .action((cmd: Command) =>
        createBitcoinWallet(
            cmd.network
        )
    );

command
    .command('compute-multisig')
    .description('Compute multisig address')
    .requiredOption('--pubkeys <pubkeys>', 'List of public keys "," separated')
    .requiredOption('--minimumSigners <minimumSigners>', 'Minimum number of signers')
    .option('--outDir <outDir>', 'The output dir', './upgrade_tx_exec.json')
    .option('--network <network>', 'network', DEFAULT_NETWORK)
    .action((cmd: Command) =>
        compute_multisig_address(
            cmd.pubkeys.split(","),
            Number(cmd.minimumSigners),
            cmd.outDir,
            cmd.network
        )
    );

command
    .command('create-upgrade-tx')
    .description('Create an unsigned multisig upgrade transaction')
    .requiredOption('--inputTxId <inputTxId>', 'The input txid used to pay fee')
    .requiredOption('--inputVout <inputVout>', 'The input vout used to pay fee')
    .requiredOption('--inputAmount <inputAmount>', 'The input amount used to pay fee')
    .requiredOption('--upgradeProposalTxId <upgradeProposalTxId>', 'The multisig witness script')
    .requiredOption('--fee <fee>', 'The transaction fee')
    .option('--outDir <outDir>', 'The output dir', './upgrade_tx_exec.json')
    .option('--network <network>', 'network', DEFAULT_NETWORK)
    .action((cmd: Command) =>
        createUnsignedUpgradeTransaction(
            {
                txid: cmd.inputTxId,
                amountSatoshis: Number(cmd.inputAmount),
                vout: Number(cmd.inputVout),
            },
            cmd.upgradeProposalTxId,
            Number(cmd.fee),
            cmd.outDir,
            cmd.network
        )
    );

command
    .command('sign-upgrade-tx')
    .description('Sign and upgrade transaction')
    .requiredOption('--privateKey <privateKey>', 'The signer private key')
    .option('--outDir <outDir>', 'The output dir', './upgrade_tx_exec.json')
    .option('--network <network>', 'network', DEFAULT_NETWORK)
    .action((cmd: Command) =>
        signPsbt(
            cmd.privateKey,
            cmd.outDir,
            cmd.network
        )
    );

command
    .command('finalize-upgrade-tx')
    .description('finalize the upgrade transaction')
    .option('--outDir <outDir>', 'The output dir', './upgrade_tx_exec.json')
    .option('--network <network>', 'network', DEFAULT_NETWORK)
    .action((cmd: Command) =>
        finalizeAndExtractTx(
            cmd.outDir,
            cmd.network
        )
    );

command
    .command('broadcast-tx')
    .description('broadcast finalized transaction')
    .requiredOption('--rpcUrl <rpcUrl>', 'The rpc url')
    .requiredOption('--rpcUser <rpcUser>', 'The rpc user')
    .requiredOption('--rpcPass <rpcPass>', 'The rpc password')
    .option('--outDir <outDir>', 'The output dir', './upgrade_tx_exec.json')
    .option('--network <network>', 'network', DEFAULT_NETWORK)
    .action((cmd: Command) =>
        broadcastTransaction(
            cmd.outDir,
            cmd.rpcUrl,
            cmd.rpcUser,
            cmd.rpcPass,
        )
    );
