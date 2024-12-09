import * as fs from 'fs/promises';
import * as dotenv from 'dotenv';

export async function updateEnvVariable(envFilePath: string, variableName: string, newValue: string) {
    const envFileContent = await fs.readFile(envFilePath, 'utf-8');
    const envConfig = dotenv.parse(envFileContent);

    envConfig[variableName] = newValue;

    let newEnvContent = '';
    for (const key in envConfig) {
        newEnvContent += `${key}=${envConfig[key]}\n`;
    }

    await fs.writeFile(envFilePath, newEnvContent, 'utf-8');
}
