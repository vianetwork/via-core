// transactions.ts

import { Command } from 'commander';
import * as utils from 'utils';

const CONTAINER_NAME = 'via-core-bitcoin-cli-1';
const RPC_USER = 'rpcuser';
const RPC_PASSWORD = 'rpcpassword';
const WALLET = 'Alice';
const RPC_ARGS = `-regtest -rpcconnect=bitcoind -rpcuser=${RPC_USER} -rpcpassword=${RPC_PASSWORD} -rpcwait -rpcwallet=${WALLET}`;

const DESTINATION_ADDRESS = 'mqdofsXHpePPGBFXuwwypAqCcXi48Xhb2f';

export const generateRandomTransactions = async () => {
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
                            `docker exec ${CONTAINER_NAME} bitcoin-cli ${RPC_ARGS} createrawtransaction "[]" "{\\"${DESTINATION_ADDRESS}\\":0.005}"`
                        );
                        unfundedTx = unfundedTxResult.stdout.trim();
                    });

                    const feeFactor = randomBetween(0, 28);
                    const randFee = (0.00001 * Math.pow(1.1892, feeFactor)).toFixed(8);
                    const options = `{"feeRate": ${randFee}}`;

                    fundTxLock = fundTxLock.then(async () => {
                        await runRpcCall(async () => {
                            const fundTxResult = await utils.exec(
                                `docker exec ${CONTAINER_NAME} bitcoin-cli ${RPC_ARGS} -named fundrawtransaction hexstring="${unfundedTx}" options='${options}'`
                            );
                            const fundTxJson = JSON.parse(fundTxResult.stdout.trim());
                            const fundedTxHex = fundTxJson.hex;

                            const signTxResult = await utils.exec(
                                `docker exec ${CONTAINER_NAME} bitcoin-cli ${RPC_ARGS} signrawtransactionwithwallet "${fundedTxHex}"`
                            );
                            const signTxJson = JSON.parse(signTxResult.stdout.trim());
                            const signedTxHex = signTxJson.hex;

                            await utils.exec(
                                `docker exec ${CONTAINER_NAME} bitcoin-cli ${RPC_ARGS} sendrawtransaction "${signedTxHex}"`
                            );
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
    .action(async () => {
        await generateRandomTransactions();
    });
