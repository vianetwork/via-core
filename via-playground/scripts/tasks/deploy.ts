import { deployContract } from './utils';
import { ethers } from 'ethers';
import { task } from 'hardhat/config';
import { getProvider } from './utils';

task('deploy', 'Deploy a crowdfunding contract')
    .addParam('amount', 'The funding goal amount')
    .setAction(async (taskArgs, hre) => {
        const provider = getProvider(hre.network.config.url, hre.network.name);
        const { amount } = taskArgs;
        const contractArtifactName = 'CrowdfundingCampaign';
        const constructorArguments = [amount];
        await deployContract(hre, contractArtifactName, constructorArguments);
    });

export default {};
