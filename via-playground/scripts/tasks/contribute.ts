import { task } from 'hardhat/config';
import { CrowdfundingCampaign, CrowdfundingCampaign__factory } from '../../typechain-types';
import * as fs from 'fs';
import { Wallet } from 'zksync-ethers';
import { getProvider } from './utils';

task('contribute', 'Contrinbute to the crowdfunding')
    .addParam('amount', 'The amount of BTC to send')
    .setAction(async (taskArgs, hre) => {
        const provider = getProvider(hre.network.config.url, hre.network.name);
        const wallet = new Wallet(process.env.PK!, provider);
        const { amount } = taskArgs;

        const config: any = JSON.parse(fs.readFileSync('config.json', 'utf-8'));
        const factory = new CrowdfundingCampaign__factory();
        const contract = factory.connect(wallet).attach(config.contract) as CrowdfundingCampaign;
        const value = amount;
        const tx = await contract.contribute({
            value
        });

        await tx.wait();

        console.log('Contributed amount:', value);
    });

export default {};
