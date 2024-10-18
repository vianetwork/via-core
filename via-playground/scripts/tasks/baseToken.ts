import { task } from 'hardhat/config';
import { getProvider } from './utils';

task('balance', 'get balance of address in BTC')
    .addParam('address', 'The address')
    .setAction(async (taskArgs, hre) => {
        const abi = [
            {
                constant: true,
                inputs: [],
                name: 'name',
                outputs: [
                    {
                        name: '',
                        type: 'string'
                    }
                ],
                payable: false,
                stateMutability: 'view',
                type: 'function'
            },
            {
                constant: true,
                inputs: [],
                name: 'symbol',
                outputs: [
                    {
                        name: '',
                        type: 'string'
                    }
                ],
                payable: false,
                stateMutability: 'view',
                type: 'function'
            },
            {
                constant: true,
                inputs: [],
                name: 'decimals',
                outputs: [
                    {
                        name: '',
                        type: 'uint8'
                    }
                ],
                payable: false,
                stateMutability: 'view',
                type: 'function'
            },
            {
                constant: true,
                inputs: [],
                name: 'totalSupply',
                outputs: [
                    {
                        name: '',
                        type: 'uint256'
                    }
                ],
                payable: false,
                stateMutability: 'view',
                type: 'function'
            },
            {
                constant: true,
                inputs: [
                    {
                        name: '_owner',
                        type: 'uint256'
                    }
                ],
                name: 'balanceOf',
                outputs: [
                    {
                        name: '',
                        type: 'uint256'
                    }
                ],
                payable: false,
                stateMutability: 'view',
                type: 'function'
            }
        ];

        const provider = getProvider(hre.network.config.url, hre.network.name);
        const baseToken = new hre.ethers.Contract('0x000000000000000000000000000000000000800a', abi, provider);
        console.log('name:', await baseToken.name());
        console.log('symbol:', await baseToken.symbol());
        console.log('decimals:', await baseToken.decimals());
        console.log('totalSupply:', await baseToken.totalSupply());
        console.log(`${taskArgs.address}: ${await baseToken.balanceOf(taskArgs.address)}`);
    });

export default {};
