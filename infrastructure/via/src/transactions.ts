import { Command } from 'commander';
import * as utils from 'utils';
import { VIA_DOCKER_COMPOSE } from './docker';

const CONTAINER_NAME = 'bitcoin-cli';
const RPC_CONNECT = 'bitcoind';
const RPC_USER = 'rpcuser';
const RPC_PASSWORD = 'rpcpassword';
const RPC_WALLET = 'Alice';
const DESTINATION_ADDRESS = 'mqdofsXHpePPGBFXuwwypAqCcXi48Xhb2f';

export interface Options {
    rpcConnect: string;
    rpcUsername: string;
    rpcPassword: string;
    rpcWallet: string;
    address: string;
    skipContainer: boolean;
}

export const generateRandomTransactions = async (cmdOptions: Options) => {
    const { rpcConnect, rpcUsername, rpcPassword, rpcWallet, address: destinationAddress, skipContainer } = cmdOptions;
    const cmdPrefix = skipContainer ? '' : `docker compose -f ${VIA_DOCKER_COMPOSE} exec ${CONTAINER_NAME}`;
    const rpcArgs = `-regtest -rpcconnect=${rpcConnect} -rpcuser=${rpcUsername} -rpcpassword=${rpcPassword} -rpcwait -rpcwallet=${rpcWallet}`;

    console.log('Generating random transactions...');

    const randomBetween = (min: number, max: number): number => {
        return Math.floor(Math.random() * (max - min + 1)) + min;
    };

    const MAX_CONCURRENT_RPC_CALLS = 10; // Adjust based on your system's capacity
    let activeRpcCalls = 0;
    const rpcQueue: (() => Promise<void>)[] = [];

    const runRpcCall = async (fn: () => Promise<void>) => {
        if (activeRpcCalls >= MAX_CONCURRENT_RPC_CALLS) {
            await new Promise<void>((resolve) => {
                rpcQueue.push(async () => {
                    await fn();
                    resolve();
                });
            });
        } else {
            activeRpcCalls++;
            try {
                await fn();
            } finally {
                activeRpcCalls--;
                if (rpcQueue.length > 0) {
                    const nextCall = rpcQueue.shift();
                    if (nextCall) {
                        runRpcCall(nextCall);
                    }
                }
            }
        }
    };

    let fundTxLock = Promise.resolve();

    for (let i = 0; i < 2; i++) {
        const numTx = randomBetween(50, 150);
        console.log(`Iteration ${i + 1}: Generating ${numTx} transactions.`);

        const txPromises = [];
        for (let j = 0; j < numTx; j++) {
            const txPromise = (async () => {
                try {
                    let unfundedTx = '';
                    await runRpcCall(async () => {
                        const unfundedTxResult = await utils.exec(
                            `${cmdPrefix} bitcoin-cli ${rpcArgs} createrawtransaction "[]" "{\\"${destinationAddress}\\":0.005}"`
                        );
                        unfundedTx = unfundedTxResult.stdout.trim();
                    });

                    const feeFactor = randomBetween(0, 28);
                    const randFee = (0.00001 * Math.pow(1.1892, feeFactor)).toFixed(8);
                    const options = `{"feeRate": ${randFee}}`;

                    fundTxLock = fundTxLock.then(async () => {
                        await runRpcCall(async () => {
                            const fundTxResult = await utils.exec(
                                `${cmdPrefix} bitcoin-cli ${rpcArgs} -named fundrawtransaction hexstring="${unfundedTx}" options='${options}'`
                            );
                            const fundTxJson = JSON.parse(fundTxResult.stdout.trim());
                            const fundedTxHex = fundTxJson.hex;

                            const signTxResult = await utils.exec(
                                `${cmdPrefix} bitcoin-cli ${rpcArgs} signrawtransactionwithwallet "${fundedTxHex}"`
                            );
                            const signTxJson = JSON.parse(signTxResult.stdout.trim());
                            const signedTxHex = signTxJson.hex;

                            await utils.exec(`${cmdPrefix} bitcoin-cli ${rpcArgs} sendrawtransaction "${signedTxHex}"`);
                        });
                    });

                    await fundTxLock;
                } catch (error) {
                    console.error('Error processing transaction:', error);
                }
            })();

            txPromises.push(txPromise);
        }

        await Promise.all(txPromises);

        console.log(`Iteration ${i + 1} completed.`);
    }

    console.log('Random transactions generation completed.');
};

export const command = new Command('transactions')
    .description('Generate random transactions on the Bitcoin regtest network.')
    .option('--rpc-connect <rpcConnect>', 'RPC connect', RPC_CONNECT)
    .option('--rpc-username <rpcUsername>', 'RPC username', RPC_USER)
    .option('--rpc-password <rpcPassword>', 'RPC password', RPC_PASSWORD)
    .option('--rpc-wallet <rpcPassword>', 'RPC wallet', RPC_WALLET)
    .option('--address <address>', 'Destination address', DESTINATION_ADDRESS)
    .option('--skip-container', 'Skip execution inside a Docker container and run directly on the host')
    .action(async (options) => {
        await generateRandomTransactions(options);
    });
