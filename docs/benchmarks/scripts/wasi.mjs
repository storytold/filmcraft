import { readFile } from 'node:fs/promises'; import { WASI } from 'node:wasi';
const wasi = new WASI({ version: 'preview1', args: ['b', ...process.argv.slice(3)] });
const m = await WebAssembly.compile(await readFile(process.argv[2]));
wasi.start(await WebAssembly.instantiate(m, wasi.getImportObject()));
