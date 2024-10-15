import { HardhatUserConfig } from 'hardhat/config';
import '@matterlabs/hardhat-zksync';
import './scripts/tasks';

const config: HardhatUserConfig = {
    defaultNetwork: 'via',
    solidity: '0.8.27',
    networks: {
        via: {
            url: 'http://0.0.0.0:3050/', // rpc url
            accounts: [`${process.env.PK}`], // wallet private key
            zksync: true,
            ethNetwork: ''
        }
    }
};

export default config;
