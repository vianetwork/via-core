import { HardhatUserConfig } from 'hardhat/config';
import '@matterlabs/hardhat-zksync';
import './scripts/tasks';
import '@matterlabs/hardhat-zksync-verify';

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
    },
    etherscan: {
        customChains: [
            {
                network: 'via',
                chainId: 270,
                urls: {
                    apiURL: 'http://127.0.0.1:3070',
                    browserURL: '' // Todo: add block explorer api
                }
            }
        ]
    }
};

export default config;
