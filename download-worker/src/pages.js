// The HTML pages people browse: the version list at `/`, and each version's files at `<version>/`,
// grouped by what they are.

const REPO = 'https://github.com/simophin/cpxy-ng';

/** Package architecture labels under openwrt/ and openwrt-apk/. */
const PACKAGE_ARCHES = {
  aarch64: 'ARM64, generic aarch64 label',
  aarch64_generic: 'ARM64, OpenWrt generic label',
  'aarch64_cortex-a53': 'ARM64 Cortex-A53, such as the GL-MT3000',
  'aarch64_cortex-a72': 'ARM64 Cortex-A72',
  'aarch64_cortex-a76': 'ARM64 Cortex-A76',
  x86_64: 'x86-64',
};

const FEED_FILES = {
  Packages: 'Feed index',
  'Packages.gz': 'Feed index, compressed',
  'Packages.sig': 'Feed index signature',
};

/** The sections of a version page, in order. */
const SECTIONS = [
  {
    id: 'android',
    title: 'Android app',
    about: 'CPXY VPN for phones and Android TV. Once installed, it updates itself from here.',
    guide: `${REPO}/blob/main/mobile/README.md`,
  },
  {
    id: 'openwrt',
    title: 'OpenWrt package',
    about:
      'Proxy, split DNS, LAN routing and a Tailscale exit node for a router. Use the native .apk on OpenWrt 25.12 or newer, or the .ipk on opkg firmware. The apk is a direct download and adds no repository.',
    guide: `${REPO}/blob/main/docs/openwrt.md`,
  },
  {
    id: 'checksums',
    title: 'Checksums',
    about: 'The SHA-256 of every file in this version. Check a download with sha256sum -c SHA256SUMS --ignore-missing.',
  },
  { id: 'other', title: 'Other files', about: '' },
];

/** Which section a file belongs in, and how its row is labelled. `path` is inside the version. */
export function describeFile(path) {
  const parts = path.split('/');
  if (path === 'android/cpxy-vpn.apk') return { section: 'android', label: 'APK, for Android 8 and later', rank: 0 };
  if (path === 'SHA256SUMS') return { section: 'checksums', label: 'SHA256SUMS', rank: 0 };
  if (parts[0] === 'openwrt' && parts.length === 3) {
    const arch = PACKAGE_ARCHES[parts[1]] ?? parts[1];
    if (parts[2].endsWith('.ipk')) return { section: 'openwrt', label: `Package, ${arch}`, rank: 0, group: parts[1] };
    if (FEED_FILES[parts[2]]) {
      const rank = 1 + Object.keys(FEED_FILES).indexOf(parts[2]);
      return { section: 'openwrt', label: `${FEED_FILES[parts[2]]}, ${parts[1]}`, rank, group: parts[1], minor: true };
    }
  }
  if (parts[0] === 'openwrt-apk' && parts.length === 3 && parts[2] === 'cpxy.apk') {
    return { section: 'openwrt', label: `Native apk, ${PACKAGE_ARCHES[parts[1]] ?? parts[1]}`, rank: 0, group: `apk-${parts[1]}` };
  }
  return { section: 'other', label: path, rank: 0 };
}

/** The checksums in a SHA256SUMS file, by path. */
export function parseSums(text) {
  const sums = new Map();
  for (const line of text.split('\n')) {
    const m = /^([0-9a-f]{64})\s+\*?(.+)$/.exec(line.trim());
    if (m) sums.set(m[2], m[1]);
  }
  return sums;
}

const escape = (s) =>
  String(s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);

const href = (path) => path.split('/').map(encodeURIComponent).join('/');

export function formatSize(bytes) {
  const units = ['B', 'KB', 'MB', 'GB'];
  let n = bytes, i = 0;
  while (n >= 1024 && i < units.length - 1) n /= 1024, i++;
  return i === 0 ? `${n} B` : `${n.toFixed(n < 10 ? 1 : 0)} ${units[i]}`;
}

const formatDate = (date) => (date ? new Date(date).toISOString().slice(0, 16).replace('T', ' ') + ' UTC' : '');

/** A tag's own build, rather than a later commit on main. */
const isTag = (version) => !version.includes('-');

const STYLE = `
:root {
  color-scheme: light dark;
  --bg: #fafaf9; --fg: #1c1917; --muted: #78716c; --line: #e7e5e4; --card: #fff;
  --accent: #2563eb; --accent-fg: #fff; --tag: #dcfce7; --tag-fg: #166534; --warn: #fef3c7; --warn-fg: #92400e;
}
@media (prefers-color-scheme: dark) {
  :root {
    --bg: #0c0a09; --fg: #e7e5e4; --muted: #a8a29e; --line: #292524; --card: #1c1917;
    --accent: #60a5fa; --accent-fg: #0c0a09; --tag: #14532d; --tag-fg: #bbf7d0; --warn: #451a03; --warn-fg: #fde68a;
  }
}
* { box-sizing: border-box; }
body { margin: 0; background: var(--bg); color: var(--fg); font: 15px/1.5 system-ui, -apple-system, sans-serif; }
main { max-width: 860px; margin: 0 auto; padding: 32px 16px 64px; }
a { color: var(--accent); text-decoration: none; }
a:hover { text-decoration: underline; }
h1 { font-size: 26px; margin: 0 0 4px; }
h2 { font-size: 18px; margin: 0; }
code, .mono { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 13px; }
.muted { color: var(--muted); }
.crumbs { margin-bottom: 20px; font-size: 14px; }
.card { background: var(--card); border: 1px solid var(--line); border-radius: 10px; padding: 16px 18px; margin-top: 16px; }
.latest { display: flex; flex-wrap: wrap; gap: 12px; align-items: center; justify-content: space-between; }
.latest .name { font-size: 22px; font-weight: 600; }
.button { display: inline-block; background: var(--accent); color: var(--accent-fg); padding: 7px 14px; border-radius: 7px; font-weight: 600; }
.button:hover { text-decoration: none; opacity: .9; }
.badge { display: inline-block; font-size: 12px; padding: 1px 8px; border-radius: 99px; background: var(--tag); color: var(--tag-fg); vertical-align: 2px; margin-left: 6px; }
.badge.warn { background: var(--warn); color: var(--warn-fg); }
table { width: 100%; border-collapse: collapse; }
td { padding: 8px 0; border-top: 1px solid var(--line); vertical-align: top; }
td + td { padding-left: 12px; }
tr:first-child td { border-top: 0; }
.num { text-align: right; white-space: nowrap; }
.section-head { display: flex; gap: 12px; align-items: baseline; justify-content: space-between; flex-wrap: wrap; }
.about { margin: 4px 0 10px; color: var(--muted); }
.file .path { color: var(--muted); word-break: break-all; }
.file.minor .label { color: var(--muted); }
.sha { color: var(--muted); font-size: 12px; word-break: break-all; }
nav.toc { display: flex; flex-wrap: wrap; gap: 6px 14px; margin-top: 12px; font-size: 14px; }
`;

function page(title, body) {
  return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>${escape(title)}</title>
<style>${STYLE}</style>
</head>
<body><main>
${body}
</main></body>
</html>
`;
}

/**
 * The home page. `versions` is newest first, each `{ name, complete, uploaded }`; `latest` is the
 * newest complete one's name, or null.
 */
export function renderIndex(versions, latest) {
  const latestCard = latest
    ? `<div class="card latest">
  <div><div class="muted">Latest version</div><div class="name">${escape(latest)}</div></div>
  <a class="button" href="/${href(latest)}/">See downloads</a>
</div>`
    : `<div class="card muted">Nothing is published yet.</div>`;

  const rows = versions
    .map(
      (v) => `<tr>
  <td><a href="/${href(v.name)}/">${escape(v.name)}</a>${isTag(v.name) ? '<span class="badge">tag</span>' : ''}${
    v.name === latest ? '<span class="badge">latest</span>' : ''
  }${v.complete ? '' : '<span class="badge warn">uploading</span>'}</td>
  <td class="num muted">${escape(formatDate(v.uploaded))}</td>
</tr>`,
    )
    .join('\n');

  return page(
    'cpxy downloads',
    `<h1>cpxy downloads</h1>
<div class="muted">Every build of <a href="${REPO}">cpxy-ng</a>'s main branch, newest first. A tag's build carries the tag's name; later commits add the commit count and hash.</div>
${latestCard}
${versions.length ? `<div class="card"><table>\n${rows}\n</table></div>` : ''}
<p class="muted">For scripts: <code>/latest</code> is the latest version's name, and <code>/latest/&lt;path&gt;</code> redirects to that file in it.</p>`,
  );
}

/**
 * A version's page. `files` are `{ path, size }` with paths inside the version, `sums` is from its
 * SHA256SUMS (empty while it uploads), `uploaded` is when SHA256SUMS arrived.
 */
export function renderVersion(version, files, sums, { complete, latest, uploaded }) {
  const bySection = new Map();
  for (const file of files) {
    const info = describeFile(file.path);
    if (!bySection.has(info.section)) bySection.set(info.section, []);
    bySection.get(info.section).push({ ...file, ...info });
  }
  for (const list of bySection.values()) {
    list.sort((a, b) => (a.group ?? '').localeCompare(b.group ?? '') || (a.rank === b.rank ? 0 : a.rank < b.rank ? -1 : 1) || a.path.localeCompare(b.path));
  }
  const sections = SECTIONS.filter((s) => bySection.has(s.id));

  const fileRow = (f) => {
    const sha = sums.get(f.path);
    return `<tr class="file${f.minor ? ' minor' : ''}">
  <td><a class="label" href="/${href(version)}/${href(f.path)}" download>${escape(f.label)}</a>
    <div class="path mono">${escape(f.path)}</div>${sha ? `\n    <div class="sha mono" title="SHA-256">${sha}</div>` : ''}</td>
  <td class="num muted">${escape(formatSize(f.size))}</td>
</tr>`;
  };

  const body = sections
    .map(
      (s) => `<section class="card" id="${s.id}">
  <div class="section-head"><h2>${escape(s.title)}</h2>${s.guide ? `<a href="${s.guide}">Guide</a>` : ''}</div>
  ${s.about ? `<p class="about">${escape(s.about)}</p>` : ''}
  <table>
${bySection.get(s.id).map(fileRow).join('\n')}
  </table>
</section>`,
    )
    .join('\n');

  const status = [
    isTag(version) ? '<span class="badge">tag</span>' : '',
    version === latest ? '<span class="badge">latest</span>' : '',
    complete ? '' : '<span class="badge warn">uploading</span>',
  ].join('');

  const toc = sections.map((s) => `<a href="#${s.id}">${escape(s.title)}</a>`).join('');

  return page(
    `cpxy ${version}`,
    `<div class="crumbs"><a href="/">All versions</a>${
      latest && latest !== version ? ` · <a href="/${href(latest)}/">Latest: ${escape(latest)}</a>` : ''
    }</div>
<h1>${escape(version)}${status}</h1>
<div class="muted">${uploaded ? `Published ${escape(formatDate(uploaded))} · ` : ''}${files.length} files${
      isTag(version) ? '' : ` · <a href="${REPO}/commit/${escape(version.split('-g').pop())}">commit</a>`
    }</div>
${complete ? '' : '<p>This version is still uploading; some files may be missing.</p>'}
${files.length ? `<nav class="toc">${toc}</nav>\n${body}` : '<div class="card muted">No files.</div>'}`,
  );
}
