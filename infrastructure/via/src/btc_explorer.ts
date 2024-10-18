import { Command } from 'commander';
import { up } from './up';
import { BTC_EXPLORER_DOCKER_COMPOSE } from './docker';




export async function bitcoin_explorer() {
    process.chdir(`${process.env.VIA_HOME}`);
    await up(BTC_EXPLORER_DOCKER_COMPOSE);

    console.log('Bitcoin explorer is running on http://localhost:1880');
}

export const command = new Command('btc-explorer').description('Running a Bitcoin explorer ').action(bitcoin_explorer);
