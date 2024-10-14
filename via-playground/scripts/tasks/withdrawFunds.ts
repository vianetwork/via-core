import { task } from 'hardhat/config';
import { CrowdfundingCampaign, CrowdfundingCampaign__factory } from '../../typechain-types';
import * as fs from 'fs';
import { Wallet } from 'zksync-ethers';
import { getProvider } from '../provider';

task('withdraw', 'Withdraw funds from the crowdfunding').setAction(async (taskArgs, hre) => {
    const provider = getProvider(hre.network.config.url, hre.network.name);
    const wallet = new Wallet(process.env.PK!, provider);

    const config: any = JSON.parse(fs.readFileSync('config.json', 'utf-8'));
    const factory = new CrowdfundingCampaign__factory();
    const contract = factory.connect(wallet).attach(config.contract) as CrowdfundingCampaign;
    const tx = await contract.withdrawFunds();
    await tx.wait();
    console.log('Withdrawen');
});

export default {};
