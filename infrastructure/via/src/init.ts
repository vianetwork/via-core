import { Command } from 'commander';

import * as utils from 'utils';
import { announced } from 'utils';

import { clean } from './clean';
import * as compiler from './compiler';
import * as contract from './contract';
import * as db from './database';
import * as docker from './docker';
import * as env from './env';
import * as config from './config';
import * as run from './run';
// import * as server from './server';
import { createVolumes, up } from './up';

// Checks if all required tools are installed with the correct versions
const checkEnv = async (): Promise<void> => {
    const tools = ['node', 'yarn', 'docker', 'cargo'];
    for (const tool of tools) {
        await utils.exec(`which ${tool}`);
    }
    const { stdout: nodeVersion } = await utils.exec('node --version');
    if ('v18.18.0' >= nodeVersion) {
        throw new Error('Error, node.js version 18.18.0 or higher is required');
    }
    const { stdout: yarnVersion } = await utils.exec('yarn --version');
    if ('1.22.0' >= yarnVersion) {
        throw new Error('Error, yarn version 1.22.0 is required');
    }
};

// Initializes and updates the git submodule
const submoduleUpdate = async (): Promise<void> => {
    await utils.exec('git submodule init');
    await utils.exec('git submodule update');
};

// Sets up docker environment and compiles contracts
type InitSetupOptions = {
    skipEnvSetup: boolean;
    skipSubmodulesCheckout: boolean;
    runObservability: boolean;
};
const initSetup = async ({
    skipSubmodulesCheckout,
    skipEnvSetup,
    runObservability
}: InitSetupOptions): Promise<void> => {
    await announced(`Initializing in 'Roll-up mode'}`);
    if (!skipSubmodulesCheckout) {
        await announced('Checkout submodules', submoduleUpdate());
    }
    if (!process.env.CI && !skipEnvSetup) {
        await announced('Pulling images', docker.pull());
        await announced('Checking environment', checkEnv());
        await announced('Checking git hooks', env.gitHooks());
        await announced('Create volumes', createVolumes());
        await announced('Setting up containers', up(docker.VIA_DOCKER_COMPOSE));
    }

    await announced('Compiling JS packages', run.yarn());

    await Promise.all([
        announced('Building L2 contracts', contract.build()),
        announced('Compile L2 system contracts', compiler.compileAll())
    ]);
};

const initDatabase = async (shouldCheck: boolean = true): Promise<void> => {
    await announced('Drop postgres db', db.drop({ core: true, prover: true }));
    await announced('Setup postgres db', db.setup({ core: true, prover: true }, shouldCheck));
    await announced('Clean rocksdb', clean(`db/${process.env.ZKSYNC_ENV!}`));
    await announced('Clean backups', clean(`backups/${process.env.ZKSYNC_ENV!}`));
};

// Deploys ERC20 and WETH tokens to localhost
// ?
// type DeployTestTokensOptions = { envFile?: string };
// const deployTestTokens = async (options?: DeployTestTokensOptions) => {
//     await announced(
//         'Deploying localhost ERC20 and Weth tokens',
//         run.deployERC20AndWeth({ command: 'dev', envFile: options?.envFile })
//     );
// };

// Deploys and verifies L1 contracts and initializes governance
const initBridgehubStateTransition = async () => {
    await announced('Reloading env', env.reload());
};

// ?
const makeEraChainIdSameAsCurrent = async () => {
    console.log('Making era chain id same as current chain id');

    const initEnv = `etc/env/l1-inits/${process.env.L1_ENV_NAME ? process.env.L1_ENV_NAME : '.init'}.env`;
    env.modify('CONTRACTS_ERA_CHAIN_ID', process.env.CHAIN_ETH_ZKSYNC_NETWORK_ID!, initEnv, false);
    env.reload();
};

// ?
const makeEraAddressSameAsCurrent = async () => {
    console.log('Making era address same as current address');
    const initEnv = `etc/env/l1-inits/${process.env.L1_ENV_NAME ? process.env.L1_ENV_NAME : '.init'}.env`;
    env.modify('CONTRACTS_ERA_DIAMOND_PROXY_ADDR', process.env.CONTRACTS_DIAMOND_PROXY_ADDR!, initEnv, false);
    env.reload();
};

// ########################### Command Actions ###########################
type InitDevCmdActionOptions = InitSetupOptions & {
    skipTestTokenDeployment?: boolean;
    skipVerifier?: boolean;
    baseTokenName?: string;
    localLegacyBridgeTesting?: boolean;
    shouldCheckPostgres: boolean; // Whether to perform `cargo sqlx prepare --check`
};
export const initDevCmdAction = async ({
    skipEnvSetup,
    skipSubmodulesCheckout,
    skipVerifier,
    skipTestTokenDeployment,
    baseTokenName,
    runObservability,
    localLegacyBridgeTesting,
    shouldCheckPostgres
}: InitDevCmdActionOptions): Promise<void> => {
    if (localLegacyBridgeTesting) {
        await makeEraChainIdSameAsCurrent();
    }
    await initSetup({
        skipEnvSetup,
        skipSubmodulesCheckout,
        runObservability
    });

    // ?
    // if (!skipTestTokenDeployment) {
    //     await deployTestTokens(testTokenOptions);
    // }
    await initBridgehubStateTransition();
    await initDatabase(shouldCheckPostgres);
    if (localLegacyBridgeTesting) {
        await makeEraAddressSameAsCurrent();
    }
};

// ########################### Command Definitions ###########################
export const initCommand = new Command('init')
    .option('--skip-submodules-checkout')
    .option('--skip-env-setup')
    .option('--skip-test-token-deployment') // ?
    .option('--base-token-name <base-token-name>', 'base token name') // ?
    // .option('--validium-mode', 'deploy contracts in Validium mode')
    .option('--run-observability', 'run observability suite')
    .option(
        '--local-legacy-bridge-testing',
        'used to test LegacyBridge compatibily. The chain will have the same id as the era chain id, while eraChainId in L2SharedBridge will be 0'
    )
    .option('--should-check-postgres', 'Whether to perform cargo sqlx prepare --check during database setup', true)
    .description('Deploys the shared bridge and registers a hyperchain locally, as quickly as possible.')
    .action(initDevCmdAction);
