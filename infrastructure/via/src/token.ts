import { Command } from 'commander';
import { Wallet, Provider, Contract } from 'zksync-ethers';
import { ethers } from 'ethers';
import * as utils from 'utils';
import {
    DEFAULT_DEPOSITOR_PRIVATE_KEY,
    DEFAULT_NETWORK,
    DEFAULT_L1_RPC_URL,
    DEFAULT_RPC_USERNAME,
    DEFAULT_RPC_PASSWORD,
    DEFAULT_L2_PRIVATE_KEY,
    DEFAULT_L2_RPC_URL,
    L2_BASE_TOKEN,
    REQUIRED_L1_TO_L2_GAS_PER_PUBDATA_BYTE,
    L1_BTC_DECIMALS
} from './constants';

export async function deposit(
    amount: number,
    receiverL2Address: string,
    senderPrivateKey: string,
    network: String,
    l1RpcUrl: string,
    l2RpcUrl: string,
    rpcUsername: string,
    rpcPassword: string
) {
    if (isNaN(amount)) {
        console.error('Error: Invalid deposit amount. Please provide a valid number.');
        return;
    }

    const amountBn = ethers.parseUnits(amount.toString(), L1_BTC_DECIMALS);
    const fee = await estimateGasFee(l2RpcUrl, amount, receiverL2Address);
    const amountWithFees = amountBn + fee;

    console.log(
        `Total amount bridged to L2 including fees: ${ethers.formatUnits(amountWithFees, L1_BTC_DECIMALS)} BTC`
    );

    process.chdir(`${process.env.VIA_HOME}`);
    await utils.spawn(
        `cargo run --example deposit -- ${amountWithFees} ${receiverL2Address} ${senderPrivateKey} ${network} ${l1RpcUrl} ${rpcUsername} ${rpcPassword}`
    );
}

async function depositWithOpReturn(
    amount: number,
    receiverL2Address: string,
    senderPrivateKey: string,
    network: String,
    l1RpcUrl: string,
    l2RpcUrl: string,
    rpcUsername: string,
    rpcPassword: string
) {
    if (isNaN(amount)) {
        console.error('Error: Invalid deposit amount. Please provide a valid number.');
        return;
    }

    const amountBn = ethers.parseUnits(amount.toString(), L1_BTC_DECIMALS);
    const fee = await estimateGasFee(l2RpcUrl, amount, receiverL2Address);
    const amountWithFees = amountBn + fee;

    console.log(
        `Total amount bridged to L2 including fees: ${ethers.formatUnits(amountWithFees, L1_BTC_DECIMALS)} BTC`
    );

    process.chdir(`${process.env.VIA_HOME}`);
    await utils.spawn(
        `cargo run --example deposit_opreturn -- ${amountWithFees} ${receiverL2Address} ${senderPrivateKey} ${network} ${l1RpcUrl} ${rpcUsername} ${rpcPassword}`
    );
}

export const getWallet = (rpcUrl: string, userL2PrivateKey: string) => {
    const provider = new Provider(rpcUrl);
    const wallet = new Wallet(userL2PrivateKey, provider);
    return wallet;
};

export async function withdraw(
    amount: string,
    receiverL1Address: string,
    userL2PrivateKey: string,
    rpcUrl: string,
    nonce?: number
) {
    if (isNaN(Number(amount))) {
        console.error('Error: Invalid withdraw amount. Please provide a valid number.');
        return;
    }

    const abi = [
        {
            inputs: [
                {
                    internalType: 'uint256',
                    name: '_account',
                    type: 'uint256'
                }
            ],
            name: 'balanceOf',
            outputs: [
                {
                    internalType: 'uint256',
                    name: '',
                    type: 'uint256'
                }
            ],
            stateMutability: 'view',
            type: 'function'
        },
        {
            inputs: [
                {
                    internalType: 'bytes',
                    name: '_l1Receiver',
                    type: 'bytes'
                }
            ],
            name: 'withdraw',
            outputs: [],
            stateMutability: 'payable',
            type: 'function'
        }
    ];

    const wallet = getWallet(rpcUrl, userL2PrivateKey);
    const btcAddress = ethers.toUtf8Bytes(receiverL1Address);
    const contract = new Contract(L2_BASE_TOKEN, abi, wallet) as any;

    let balance = await contract.balanceOf(wallet.address);
    console.log('Balance before withdraw', ethers.formatEther(String(balance)));
    const data: any = { value: ethers.parseEther(String(amount)) };
    if (nonce) {
        data['nonce'] = nonce;
    }

    const tx = await contract.connect(wallet).withdraw(btcAddress, data);
    await tx.wait();
    balance = await contract.balanceOf(wallet.address);
    console.log('Balance after withdraw', ethers.formatEther(String(balance)));
}

async function estimateGasFee(l2RpcUrl: string, amount: number, receiverL2Address: string): Promise<bigint> {
    const l2Provider = new Provider(l2RpcUrl);
    const amountBn = ethers.parseUnits(amount.toString(), L1_BTC_DECIMALS);

    const gasCost = BigInt(
        await l2Provider.estimateL1ToL2Execute({
            contractAddress: receiverL2Address,
            calldata: '0x',
            caller: receiverL2Address,
            factoryDeps: [],
            gasPerPubdataByte: REQUIRED_L1_TO_L2_GAS_PER_PUBDATA_BYTE,
            l2Value: amountBn
        })
    );

    // https://github.com/vianetwork/via-core/blob/7c73be01d7160320c615ce0d70bdcdb2c8a9671c/core/lib/types/src/l1/via_l1.rs#L15
    // Hardcode the gas price to avoid issues during the Priority Id verification.
    // const gasPrice = await l2Provider.getGasPrice();
    const gasPrice = BigInt(120_000_000);
    return (gasCost * gasPrice) / BigInt(10_000_000_000);
}

export const command = new Command('token').description('Bridge BTC L2<>L1');
command
    .command('deposit')
    .description('deposit BTC to l2')
    .requiredOption('--amount <amount>', 'amount of BTC to deposit', parseFloat)
    .requiredOption('--receiver-l2-address <receiverL2Address>', 'receiver l2 address')
    .option('--sender-private-key <senderPrivateKey>', 'sender private key', DEFAULT_DEPOSITOR_PRIVATE_KEY)
    .option('--network <network>', 'network', DEFAULT_NETWORK)
    .option('--l1-rpc-url <l1RcpUrl>', 'RPC URL', DEFAULT_L1_RPC_URL)
    .option('--l2-rpc-url <l2RcpUrl>', 'RPC URL', DEFAULT_L2_RPC_URL)
    .option('--rpc-username <rcpUsername>', 'RPC username', DEFAULT_RPC_USERNAME)
    .option('--rpc-password <rpcPassword>', 'RPC password', DEFAULT_RPC_PASSWORD)
    .action((cmd: Command) =>
        deposit(
            cmd.amount,
            cmd.receiverL2Address,
            cmd.senderPrivateKey,
            cmd.network,
            cmd.l1RpcUrl,
            cmd.l2RpcUrl,
            cmd.rpcUsername,
            cmd.rpcPassword
        )
    );

command
    .command('deposit-with-op-return')
    .description('deposit BTC to l2 with op-return')
    .requiredOption('--amount <amount>', 'amount of BTC to deposit', parseFloat)
    .requiredOption('--receiver-l2-address <receiverL2Address>', 'receiver l2 address')
    .option('--sender-private-key <senderPrivateKey>', 'sender private key', DEFAULT_DEPOSITOR_PRIVATE_KEY)
    .option('--network <network>', 'network', DEFAULT_NETWORK)
    .option('--l1-rpc-url <l1RcpUrl>', 'RPC URL', DEFAULT_L1_RPC_URL)
    .option('--l2-rpc-url <l2RcpUrl>', 'RPC URL', DEFAULT_L2_RPC_URL)
    .option('--rpc-username <rcpUsername>', 'RPC username', DEFAULT_RPC_USERNAME)
    .option('--rpc-password <rpcPassword>', 'RPC password', DEFAULT_RPC_PASSWORD)
    .action((cmd: Command) =>
        depositWithOpReturn(
            cmd.amount,
            cmd.receiverL2Address,
            cmd.senderPrivateKey,
            cmd.network,
            cmd.l1RpcUrl,
            cmd.l2RpcUrl,
            cmd.rpcUsername,
            cmd.rpcPassword
        )
    );

command
    .command('withdraw')
    .description('withdraw BTC to l1')
    .requiredOption('--amount <amount>', 'amount of BTC to withdraw')
    .requiredOption('--receiver-l1-address <receiverL1Address>', 'receiver l1 address')
    .option('--user-private-key <userPrivateKey>', 'user private key', DEFAULT_L2_PRIVATE_KEY)
    .option('--rpc-url <rcpUrl>', 'RPC URL', DEFAULT_L2_RPC_URL)
    .action((cmd: Command) => withdraw(cmd.amount, cmd.receiverL1Address, cmd.userPrivateKey, cmd.rpcUrl));
