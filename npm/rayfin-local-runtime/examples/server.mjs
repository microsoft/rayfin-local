import { createInterface } from 'node:readline/promises';
import { startRayfinLocal } from '@microsoft/rayfin-local-runtime/server';

if (!process.argv[2]) throw new Error('Usage: node server.mjs <Rayfin project directory>');
const server = await startRayfinLocal({ projectDir: process.argv[2] });
const terminal = createInterface({ input: process.stdin, output: process.stdout });
try {
  console.log(`Rayfin API: ${server.apiUrl}`);
  await terminal.question('Press Enter to stop.\n');
} finally {
  terminal.close();
  await server.stop();
}
