// transactions.ts

import { Command } from 'commander';
import * as utils from 'utils';

const CONTAINER_NAME = 'via-core-bitcoin-cli-1';

export const generateRandomTransactions = async () => {
    console.log('Generating random transactions...');

    const randomBetween = (min: number, max: number): number => {
        return Math.floor(Math.random() * (max - min + 1)) + min;
    };

    for (let i = 0; i < 10; i++) {
        const numTx = randomBetween(50, 150);
        console.log(`Iteration ${i + 1}: Generating ${numTx} transactions.`);

        for (let j = 0; j < numTx; j++) {
            const newAddressResult = await utils.exec(
                `docker exec ${CONTAINER_NAME} bitcoin-cli -regtest -rpcconnect=bitcoind -rpcuser=rpcuser -rpcpassword=rpcpassword getnewaddress`
            );
            const newAddress = newAddressResult.stdout.trim();

            const unfundedTxResult = await utils.exec(
                `docker exec ${CONTAINER_NAME} bitcoin-cli -regtest -rpcconnect=bitcoind -rpcuser=rpcuser -rpcpassword=rpcpassword createrawtransaction "[]" "{\\"${newAddress}\\":0.005}"`
            );
            const unfundedTx = unfundedTxResult.stdout.trim();

            const feeFactor = randomBetween(0, 28);
            const randFee = (0.00001 * Math.pow(1.1892, feeFactor)).toFixed(8);

            const options = `{"feeRate": ${randFee}}`;

            const fundTxResult = await utils.exec(
                `docker exec ${CONTAINER_NAME} bitcoin-cli -regtest -rpcconnect=bitcoind -rpcuser=rpcuser -rpcpassword=rpcpassword -named fundrawtransaction hexstring="${unfundedTx}" options='${options}'`
            );
            const fundTxJson = JSON.parse(fundTxResult.stdout.trim());
            const fundedTxHex = fundTxJson.hex;

            const signTxResult = await utils.exec(
                `docker exec ${CONTAINER_NAME} bitcoin-cli -regtest -rpcconnect=bitcoind -rpcuser=rpcuser -rpcpassword=rpcpassword signrawtransactionwithwallet "${fundedTxHex}"`
            );
            const signTxJson = JSON.parse(signTxResult.stdout.trim());
            const signedTxHex = signTxJson.hex;

            await utils.exec(
                `docker exec ${CONTAINER_NAME} bitcoin-cli -regtest -rpcconnect=bitcoind -rpcuser=rpcuser -rpcpassword=rpcpassword sendrawtransaction "${signedTxHex}"`
            );
        }

        console.log(`Iteration ${i + 1} completed.`);
    }

    console.log('Random transactions generation completed.');
};

export const command = new Command('transactions')
    .description('Generate random transactions on the Bitcoin regtest network.')
    .action(async () => {
        await generateRandomTransactions();
    });
