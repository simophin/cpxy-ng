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
    uploaded: new Date('2026-10-15T03:00:00Z'),
    writeHttpMetadata() {},
  });
  return {
    async head(key) {
      return key in files ? object(key) : null;
    },
    async get(key, options = {}) {
      if (!(key in files)) return null;
      const body = { ...object(key), body: files[key], text: async () => files[key] };
      // Like R2, a range from headers is set with or without a Range header
      if (options.range instanceof Headers) {
        const match = /^bytes=(\d+)-(\d*)$/.exec(options.range.get('range') ?? '');
        const offset = match ? +match[1] : 0;
        const end = match?.[2] ? +match[2] + 1 : files[key].length;
        return { ...body, body: files[key].slice(offset, end), range: { offset, length: end - offset } };
      }
      return body;
    },
    async list({ delimiter, prefix, cursor }) {
      const start = cursor ? +cursor : 0;
      const end = start + pageSize;
      if (prefix !== undefined) {
        const keys = Object.keys(files).filter((k) => k.startsWith(prefix)).sort();
        return { objects: keys.slice(start, end).map(object), truncated: end < keys.length, cursor: String(end) };
      }
      assert.equal(delimiter, '/');
      const prefixes = [...new Set(Object.keys(files).map((k) => k.split('/')[0] + '/'))].sort();
      return {
        delimitedPrefixes: prefixes.slice(start, end),
        truncated: end < prefixes.length,
        cursor: String(end),
      };
    },
  };
}

const get = (bucket, path, method = 'GET', headers = {}) =>
  worker.fetch(new Request(`https://dl.example${path}`, { method, headers, redirect: 'manual' }), { BUCKET: bucket });
const browse = (bucket, path) => get(bucket, path, 'GET', { accept: 'text/html,application/xhtml+xml,*/*;q=0.8' });

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

test('serves a range only when asked for one', async () => {
  const bucket = fakeBucket({ '1.17.0/SHA256SUMS': 'sums' });
  const whole = await get(bucket, '/1.17.0/SHA256SUMS');
  assert.equal(whole.status, 200);
  assert.equal(whole.headers.get('content-range'), null);
  assert.equal(await whole.text(), 'sums');

  const part = await get(bucket, '/1.17.0/SHA256SUMS', 'GET', { range: 'bytes=1-2' });
  assert.equal(part.status, 206);
  assert.equal(part.headers.get('content-range'), 'bytes 1-2/4');
  assert.equal(await part.text(), 'um');
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

const SUMS = (paths) => paths.map((p, i) => `${String(i).padStart(64, '0')}  ${p}`).join('\n') + '\n';

test('lists versions as text for scripts and as a page for browsers', async () => {
  const bucket = fakeBucket({
    '1.17.0/SHA256SUMS': 'x',
    '1.17.0-2-gabc1234/SHA256SUMS': 'x',
    '1.17.0-3-gdef5678/android/cpxy-vpn.apk': 'x',
  });
  assert.equal(await (await get(bucket, '/')).text(), '1.17.0-3-gdef5678\n1.17.0-2-gabc1234\n1.17.0\n');

  const page = await browse(bucket, '/');
  assert.match(page.headers.get('content-type'), /text\/html/);
  assert.equal(page.headers.get('vary'), 'accept');
  const html = await page.text();
  assert.match(html, /Latest version<\/div><div class="name">1\.17\.0-2-gabc1234</);
  assert.match(html, /href="\/1\.17\.0-3-gdef5678\/">1\.17\.0-3-gdef5678<\/a><span class="badge warn">uploading/);
  assert.match(html, /href="\/1\.17\.0\/">1\.17\.0<\/a><span class="badge">tag/);
});

test('a version page lists every file under its section, with its checksum', async () => {
  const paths = [
    'android/cpxy-vpn.apk',
    'openwrt/x86_64/cpxy.ipk',
    'openwrt/x86_64/Packages',
    'raw/x86_64-pc-windows-msvc/cpxy-client.exe',
    'raw/x86_64-unknown-linux-musl/cpxy-client',
    'raw/x86_64-unknown-linux-musl/cpxy-tsgw',
    'notes.txt',
  ];
  const files = Object.fromEntries(paths.map((p) => [`1.17.0/${p}`, 'x'.repeat(2048)]));
  files['1.17.0/SHA256SUMS'] = SUMS(paths);
  const bucket = fakeBucket(files);

  const response = await browse(bucket, '/1.17.0/');
  assert.equal(response.status, 200);
  const html = await response.text();
  for (const p of [...paths, 'SHA256SUMS']) assert.ok(html.includes(`href="/1.17.0/${p}"`), p);
  const order = ['id="android"', 'id="openwrt"', 'id="cpxy-client"', 'id="cpxy-tsgw"', 'id="checksums"', 'id="other"'];
  const at = order.map((s) => html.indexOf(s));
  assert.ok(at.every((i, n) => i > 0 && (n === 0 || i > at[n - 1])), `sections in order: ${at}`);
  assert.match(html, />Windows x64</);
  assert.match(html, />Linux x86-64</);
  assert.match(html, /Package, x86-64/);
  assert.ok(html.includes('0'.repeat(63) + '1'), 'checksum of the second file');
  assert.match(html, /2\.0 KB/);
});

test('version pages redirect to their folder, and 404 when empty', async () => {
  const bucket = fakeBucket({ '1.17.0/SHA256SUMS': 'x' });
  const response = await browse(bucket, '/1.17.0');
  assert.equal(response.status, 301);
  assert.equal(response.headers.get('location'), '/1.17.0/');
  assert.equal((await browse(bucket, '/1.18.0/')).status, 404);
});

test('latest/ takes a browser to the latest version page', async () => {
  const bucket = fakeBucket({ '1.17.0/SHA256SUMS': 'x' });
  const response = await browse(bucket, '/latest/');
  assert.equal(response.status, 302);
  assert.equal(response.headers.get('location'), '/1.17.0/');
  assert.equal(await (await get(bucket, '/latest/')).text(), '1.17.0\n');
});

test('escapes file names', async () => {
  const bucket = fakeBucket({ '1.17.0/SHA256SUMS': 'x', '1.17.0/<b>.txt': 'x' });
  const html = await (await browse(bucket, '/1.17.0/')).text();
  assert.ok(!html.includes('<b>'));
  assert.ok(html.includes('&lt;b&gt;.txt'));
});
