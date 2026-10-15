// Signs a file as `usign -S -m <file> -s <key>` does, writing <file>.sig. opkg checks a feed's
// Packages.sig against the keys in /etc/opkg/keys. CI verifies the result with OpenWrt's usign.
//
// usage: node usign-sign.mjs <file> <secret key file>
import { createPrivateKey, sign } from 'node:crypto';
import { readFileSync, writeFileSync } from 'node:fs';

const [file, keyFile] = process.argv.slice(2);
if (!file || !keyFile) {
  console.error('usage: node usign-sign.mjs <file> <secret key file>');
  process.exit(2);
}

// "Ed" "BK" kdfrounds(4) salt(16) checksum(8) fingerprint(8) seed(32) public key(32)
const key = Buffer.from(readFileSync(keyFile, 'utf8').trim().split('\n').pop(), 'base64');
if (key.length !== 104 || key.toString('latin1', 0, 4) !== 'EdBK' || key.readUInt32BE(4) !== 0) {
  console.error('not an unencrypted usign secret key');
  process.exit(1);
}
const fingerprint = key.subarray(32, 40);
const seed = key.subarray(40, 72);
const pkcs8 = Buffer.concat([Buffer.from('302e020100300506032b657004220420', 'hex'), seed]);
const signature = sign(null, readFileSync(file), createPrivateKey({ key: pkcs8, format: 'der', type: 'pkcs8' }));

const blob = Buffer.concat([Buffer.from('Ed'), fingerprint, signature]);
writeFileSync(`${file}.sig`,
  `untrusted comment: signed by key ${fingerprint.toString('hex')}\n${blob.toString('base64')}\n`);
