import { afterAll, describe, expect, it } from 'bun:test';
import { mkdirSync, readFileSync, rmSync } from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { runInNewContext } from 'node:vm';
import { crossWrite } from './compat';

const TEST_DIR = path.join(os.tmpdir(), `compat-test-${process.pid}`);

afterAll(() => {
  rmSync(TEST_DIR, { recursive: true, force: true });
});

function testFile(name: string): string {
  const dir = path.join(TEST_DIR, name);
  mkdirSync(dir, { recursive: true });
  return path.join(dir, 'out.bin');
}

describe('crossWrite', () => {
  it('writes string data as utf-8 bytes', async () => {
    const filePath = testFile('string');
    await crossWrite(filePath, 'hello');
    expect(readFileSync(filePath)).toEqual(Buffer.from('hello'));
  });

  it('writes Buffer slices without parent-buffer bytes', async () => {
    const filePath = testFile('buffer-slice');
    const parent = Buffer.from([0xaa, 0x01, 0x02, 0x03, 0xbb]);
    const slice = parent.subarray(1, 4);
    expect(Buffer.isBuffer(slice)).toBe(true);
    await crossWrite(filePath, slice);
    expect(readFileSync(filePath)).toEqual(Buffer.from([0x01, 0x02, 0x03]));
  });

  it('writes same-realm ArrayBuffer contents', async () => {
    const filePath = testFile('arraybuffer');
    const ab = new ArrayBuffer(3);
    new Uint8Array(ab).set([0x10, 0x20, 0x30]);
    await crossWrite(filePath, ab);
    expect(readFileSync(filePath)).toEqual(Buffer.from([0x10, 0x20, 0x30]));
  });

  it('writes cross-realm ArrayBuffer that fails instanceof ArrayBuffer', async () => {
    const filePath = testFile('cross-realm-ab');

    const crossRealm = runInNewContext(
      'const b = new ArrayBuffer(2); new Uint8Array(b).set([0x7e, 0x7f]); b',
    ) as ArrayBuffer;

    // Prerequisite: node:vm must yield a true cross-realm buffer.
    expect(crossRealm instanceof ArrayBuffer).toBe(false);

    await crossWrite(filePath, crossRealm);
    expect(readFileSync(filePath)).toEqual(Buffer.from([0x7e, 0x7f]));
  });
});
