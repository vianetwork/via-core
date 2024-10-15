import { deployContract } from './utils';
import { task } from 'hardhat/config';

task('deploy', 'Deploy a crowdfunding contract')
    .addParam('amount', 'The funding goal amount')
    .setAction(async (taskArgs, hre) => {
        const { amount } = taskArgs;
        const contractArtifactName = 'CrowdfundingCampaign';
        const constructorArguments = [amount];
        await deployContract(hre, contractArtifactName, constructorArguments);
    });

export default {};
