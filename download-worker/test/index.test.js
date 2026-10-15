// cd download-worker && npm test
import assert from 'node:assert/strict';
import { beforeEach, test } from 'node:test';
import worker, { compareVersions, latestVersion, parseVersion, resetCache } from '../src/index.js';

/** Enough of an R2 bucket binding for the worker, over a map of key to content. */
function fakeBucket(files, pageSize = 2) {
  const object = (key) => ({
    key,
    size: files[key].length,
    httpEtag: `"${key}"`,
    writeHttpMetadata() {},
  });
  return {
    async head(key) {
      return key in files ? object(key) : null;
    },
    async get(key) {
      return key in files ? { ...object(key), body: files[key] } : null;
    },
    async list({ delimiter, cursor }) {
      assert.equal(delimiter, '/');
      const prefixes = [...new Set(Object.keys(files).map((k) => k.split('/')[0] + '/'))].sort();
      const start = cursor ? +cursor : 0;
      const end = start + pageSize;
      return {
        delimitedPrefixes: prefixes.slice(start, end),
        truncated: end < prefixes.length,
        cursor: String(end),
      };
    },
  };
}

const get = (bucket, path, method = 'GET') =>
  worker.fetch(new Request(`https://dl.example${path}`, { method }), { BUCKET: bucket });

beforeEach(resetCache);

test('parses the version names CI makes', () => {
  assert.deepEqual(parseVersion('1.17.0'), [1, 17, 0, 0]);
  assert.deepEqual(parseVersion('1.17.0-5-gabc1234'), [1, 17, 0, 5]);
  for (const name of ['dev', 'abc1234', '1.17', '1.17.0-rc1', 'latest']) assert.equal(parseVersion(name), null, name);
});

test('orders commits after their tag, and numerically', () => {
  const sorted = ['1.9.0', '1.17.0-10-gaaaaaaa', '1.17.0', '1.17.0-9-gbbbbbbb', '1.18.0', '1.10.0'].sort(compareVersions);
  assert.deepEqual(sorted, ['1.9.0', '1.10.0', '1.17.0', '1.17.0-9-gbbbbbbb', '1.17.0-10-gaaaaaaa', '1.18.0']);
});

test('latest is the newest complete version, across list pages', async () => {
  const bucket = fakeBucket({
    '1.17.0/SHA256SUMS': 'x',
    '1.17.0-9-gbbbbbbb/SHA256SUMS': 'x',
    '1.17.0-10-gaaaaaaa/SHA256SUMS': 'x',
    // Still uploading
    '1.17.0-11-gccccccc/android/cpxy-vpn.apk': 'x',
    'scratch/file': 'x',
  });
  assert.equal(await latestVersion(bucket), '1.17.0-10-gaaaaaaa');
});

test('redirects latest paths to the version folder', async () => {
  const bucket = fakeBucket({ '1.17.0-5-gabc1234/SHA256SUMS': 'x', '1.17.0-5-gabc1234/android/cpxy-vpn.apk': 'apk' });
  for (const method of ['GET', 'HEAD']) {
    const response = await get(bucket, '/latest/android/cpxy-vpn.apk', method);
    assert.equal(response.status, 302);
    assert.equal(response.headers.get('location'), '/1.17.0-5-gabc1234/android/cpxy-vpn.apk');
  }
  const name = await get(bucket, '/latest');
  assert.equal(await name.text(), '1.17.0-5-gabc1234\n');
});

test('serves files', async () => {
  const bucket = fakeBucket({ '1.17.0/SHA256SUMS': 'sums', '1.17.0/raw/x86_64-unknown-linux-musl/cpxy-server': 'binary' });
  const response = await get(bucket, '/1.17.0/raw/x86_64-unknown-linux-musl/cpxy-server');
  assert.equal(response.status, 200);
  assert.equal(await response.text(), 'binary');
  assert.match(response.headers.get('cache-control'), /immutable/);

  const head = await get(bucket, '/1.17.0/SHA256SUMS', 'HEAD');
  assert.equal(head.status, 200);
  assert.equal(head.headers.get('content-length'), '4');
});

test('answers 404 for what is not there', async () => {
  assert.equal((await get(fakeBucket({}), '/latest/android/cpxy-vpn.apk')).status, 404);
  assert.equal((await get(fakeBucket({ '1.17.0/SHA256SUMS': 'x' }), '/1.17.0/nothing')).status, 404);
});

test('refuses other methods and odd paths', async () => {
  const bucket = fakeBucket({});
  assert.equal((await get(bucket, '/x', 'POST')).status, 405);
  assert.equal((await get(bucket, '/%E0%A4%A')).status, 400);
});
