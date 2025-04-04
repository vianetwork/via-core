import fs from 'fs';

export const DEFAULT_UPGRADE_PATH = process.env.VIA_HOME + '/etc/upgrades';
export const DEFAULT_L2CONTRACTS_FOR_UPGRADE_PATH = process.env.VIA_HOME + '/contracts/l2-contracts/contracts/upgrades';

export function getTimestampInSeconds() {
    return Math.floor(Date.now() / 1000);
}

export function getL2UpgradeFileName(environment): string {
    return getUpgradePath(environment) + '/l2Upgrade.json';
}

export function getNameOfTheLastUpgrade(): string {
    return fs.readdirSync(DEFAULT_UPGRADE_PATH).sort().reverse()[0];
}

export function getCommonUpgradePath(): string {
    const currentUpgrade = getNameOfTheLastUpgrade();
    return `${DEFAULT_UPGRADE_PATH}/${currentUpgrade}/`;
}

export function getUpgradePath(environment: string): string {
    const upgradeEnvironment = environment ?? 'localhost';
    const path = `${getCommonUpgradePath()}${upgradeEnvironment}`;
    if (!fs.existsSync(path)) {
        fs.mkdirSync(path, { recursive: true });
    }
    return path;
}
