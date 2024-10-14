import { HardhatUserConfig } from 'hardhat/config';
import '@matterlabs/hardhat-zksync';
import '@nomicfoundation/hardhat-toolbox';
import './scripts/tasks';

const config: HardhatUserConfig = {
    solidity: '0.8.27',
    networks: {
        via: {
            url: 'http://0.0.0.0:3050/', // Infura endpoint
            accounts: [`${process.env.PK}`], // Your wallet private key
            zksync: true,
            ethNetwork: ''
        }
    }
};

export default config;
