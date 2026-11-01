import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import { readFilePart } from '../src/lib/fileParts.ts';

test('image previews are permitted by the production content policy', async () => {
  const config = JSON.parse(await readFile(new URL('../../src-tauri/tauri.conf.json', import.meta.url), 'utf8'));
  const images = config.app.security.csp.split(';').find((item) => item.trim().startsWith('img-src '));
  assert.ok(images.split(/\s+/).includes('blob:'), 'local File previews must load in the packaged app');
  assert.ok(images.split(/\s+/).includes('data:'), 'received chat images must load');
});

test('file reads contain exactly the requested slice, including binary and empty data', async (t) => {
  const reads = [];
  const original = globalThis.FileReader;
  t.after(() => { if (original === undefined) delete globalThis.FileReader; else globalThis.FileReader = original; });
  globalThis.FileReader = class {
    result = null;
    async readAsDataURL(blob) {
      reads.push(blob.size);
      this.result = `data:application/octet-stream;base64,${Buffer.from(await blob.arrayBuffer()).toString('base64')}`;
      this.onload();
    }
  };
  const file = new Blob([new Uint8Array([0, 255, 128, 1, 2, 3, 4])]);
  assert.deepEqual(Buffer.from(await readFilePart(file.slice(1, 4)), 'base64'), Buffer.from([255, 128, 1]));
  assert.equal(await readFilePart(file.slice(7, 7)), '');
  assert.deepEqual(reads, [3, 0]);
});

test('file read failures produce a useful message and never return partial data', async (t) => {
  const original = globalThis.FileReader;
  t.after(() => { if (original === undefined) delete globalThis.FileReader; else globalThis.FileReader = original; });
  globalThis.FileReader = class { readAsDataURL() { this.onerror(); } };
  await assert.rejects(readFilePart(new Blob(['binary'])), /Não foi possível ler o arquivo selecionado/);
});
