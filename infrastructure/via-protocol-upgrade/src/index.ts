import { program } from 'commander';

import { command as publish } from './l2upgrade/system-contracts';
import { command as manager } from './protocol-upgrade-manager';
import { command as l2Upgrade } from './l2upgrade/transactions';

const COMMANDS = [publish, manager, l2Upgrade];

async function main() {
    const VIA_HOME = process.env.VIA_HOME;

    if (!VIA_HOME) {
        throw new Error('Please set $VIA_HOME to the root of Via repo!');
    } else {
        process.chdir(VIA_HOME);
    }

    program.version('0.1.0').name('via').description('via protocol upgrade tools');

    for (const command of COMMANDS) {
        program.addCommand(command);
    }
    await program.parseAsync(process.argv);
}

main().catch((err: Error) => {
    console.error('Error:', err.message || err);
    process.exitCode = 1;
});
