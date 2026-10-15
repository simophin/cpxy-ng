// The download server: serves the R2 bucket CI publishes to, and redirects `latest/<path>` to
// `<version>/<path>` in the newest complete version folder. A folder is complete once CI has
// uploaded its SHA256SUMS, which it does last.
//
//   <version>/SHA256SUMS
//   <version>/android/cpxy-vpn.apk
//   <version>/openwrt/<opkg arch>/{cpxy.ipk,Packages,Packages.gz,Packages.sig}
//   <version>/raw/<rust target>/cpxy-*
//
// Browsers get HTML pages (pages.js): the versions at `/` and a version's files at `<version>/`.
// Anything else asking for `/` gets the version names as text.

import { parseSums, renderIndex, renderVersion } from './pages.js';

// A tag, or `git describe` of a later commit: 1.17.0, 1.17.0-5-gabc1234. Matches AppVersion in
// mobile/shared, which orders versions the same way.
const VERSION = /^v?(\d{1,9})\.(\d{1,9})\.(\d{1,9})(?:-(\d{1,9})-g[0-9a-f]+)?$/;

/** [major, minor, patch, commits since the tag], or null for other names. */
export function parseVersion(name) {
  const m = VERSION.exec(name);
  return m ? [+m[1], +m[2], +m[3], +(m[4] ?? 0)] : null;
}

export function compareVersions(a, b) {
  const x = parseVersion(a), y = parseVersion(b);
  for (let i = 0; i < 4; i++) if (x[i] !== y[i]) return x[i] - y[i];
  return 0;
}

/** The version folders, newest first. */
async function listVersions(bucket) {
  const names = [];
  let cursor;
  do {
    const page = await bucket.list({ delimiter: '/', cursor });
    for (const prefix of page.delimitedPrefixes) {
      const name = prefix.slice(0, -1);
      if (parseVersion(name)) names.push(name);
    }
    cursor = page.truncated ? page.cursor : undefined;
  } while (cursor);
  return names.sort(compareVersions).reverse();
}

export async function latestVersion(bucket) {
  for (const name of await listVersions(bucket)) {
    if (await bucket.head(`${name}/SHA256SUMS`)) return name;
  }
  return null;
}

// Each isolate remembers the answer briefly, so a burst of requests lists the bucket once.
const LATEST_TTL_MS = 30_000;
let cached = { at: 0, version: null };

async function cachedLatest(bucket) {
  if (Date.now() - cached.at > LATEST_TTL_MS) cached = { at: Date.now(), version: await latestVersion(bucket) };
  return cached.version;
}

export function resetCache() {
  cached = { at: 0, version: null };
}

/** Every version with whether it is complete and when it was, newest first. */
async function versionStatuses(bucket) {
  return Promise.all(
    (await listVersions(bucket)).map(async (name) => {
      const sums = await bucket.head(`${name}/SHA256SUMS`);
      return { name, complete: !!sums, uploaded: sums?.uploaded };
    }),
  );
}

/** The files in a version folder, with paths inside it. */
async function listFiles(bucket, version) {
  const prefix = `${version}/`;
  const files = [];
  let cursor;
  do {
    const page = await bucket.list({ prefix, cursor });
    for (const object of page.objects) files.push({ path: object.key.slice(prefix.length), size: object.size });
    cursor = page.truncated ? page.cursor : undefined;
  } while (cursor);
  return files;
}

async function versionPage(bucket, version) {
  const [files, sums, latest] = await Promise.all([
    listFiles(bucket, version),
    bucket.get(`${version}/SHA256SUMS`),
    cachedLatest(bucket),
  ]);
  if (!files.length) return text(404, 'Not found\n');
  const html = renderVersion(version, files, sums ? parseSums(await sums.text()) : new Map(), {
    complete: !!sums,
    latest,
    uploaded: sums?.uploaded,
  });
  // A complete version does not change, but the latest marker on it does
  return new Response(html, {
    headers: { 'content-type': 'text/html; charset=utf-8', 'cache-control': sums ? 'public, max-age=300' : 'no-cache' },
  });
}

const wantsHtml = (request) => (request.headers.get('accept') ?? '').includes('text/html');

function text(status, body, headers = {}) {
  return new Response(body, { status, headers: { 'content-type': 'text/plain; charset=utf-8', ...headers } });
}

async function serveObject(request, bucket, key) {
  const head = request.method === 'HEAD';
  const object = head
    ? await bucket.head(key)
    : await bucket.get(key, { range: request.headers, onlyIf: request.headers });
  if (!object) return text(404, 'Not found\n');

  const headers = new Headers();
  object.writeHttpMetadata(headers);
  headers.set('etag', object.httpEtag);
  headers.set('accept-ranges', 'bytes');
  // Version folders never change once complete
  if (!headers.has('cache-control')) headers.set('cache-control', 'public, max-age=31536000, immutable');

  if (head) {
    headers.set('content-length', String(object.size));
    return new Response(null, { headers });
  }
  // onlyIf did not match: an object without a body
  if (!('body' in object)) return new Response(null, { status: 304, headers });
  if (object.range) {
    const offset = object.range.offset ?? object.size - object.range.suffix;
    const length = object.range.length ?? object.size - offset;
    headers.set('content-range', `bytes ${offset}-${offset + length - 1}/${object.size}`);
    headers.set('content-length', String(length));
    return new Response(object.body, { status: 206, headers });
  }
  headers.set('content-length', String(object.size));
  return new Response(object.body, { headers });
}

export default {
  async fetch(request, env) {
    if (request.method !== 'GET' && request.method !== 'HEAD') {
      return text(405, 'Method not allowed\n', { allow: 'GET, HEAD' });
    }
    const url = new URL(request.url);
    let path;
    try {
      path = decodeURIComponent(url.pathname);
    } catch {
      return text(400, 'Bad path\n');
    }
    if (path.split('/').some((part) => part === '..')) return text(400, 'Bad path\n');

    // /latest is the version name itself, for scripts
    if (path === '/latest' || path.startsWith('/latest/')) {
      const version = await cachedLatest(env.BUCKET);
      if (!version) return text(404, 'Nothing is published yet\n');
      const rest = path.slice('/latest'.length);
      if (rest === '/' && wantsHtml(request)) {
        return new Response(null, { status: 302, headers: { location: `/${version}/`, 'cache-control': 'no-cache' } });
      }
      if (rest === '' || rest === '/') return text(200, `${version}\n`, { 'cache-control': 'no-cache' });
      return new Response(null, {
        status: 302,
        headers: { location: `/${version}${rest}`, 'cache-control': 'no-cache' },
      });
    }

    if (path === '/') {
      const vary = { vary: 'accept', 'cache-control': 'no-cache' };
      if (!wantsHtml(request)) {
        const versions = await listVersions(env.BUCKET);
        return text(200, versions.map((v) => `${v}\n`).join(''), vary);
      }
      const versions = await versionStatuses(env.BUCKET);
      const latest = versions.find((v) => v.complete)?.name ?? null;
      return new Response(renderIndex(versions, latest), {
        headers: { 'content-type': 'text/html; charset=utf-8', ...vary },
      });
    }

    const name = path.slice(1).replace(/\/$/, '');
    if (parseVersion(name)) {
      if (!path.endsWith('/')) return new Response(null, { status: 301, headers: { location: `/${name}/` } });
      return versionPage(env.BUCKET, name);
    }
    if (path.endsWith('/')) return text(404, 'Not found\n');
    return serveObject(request, env.BUCKET, path.slice(1));
  },
};
