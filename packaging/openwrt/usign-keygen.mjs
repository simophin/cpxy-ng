// Generates a usign (OpenWrt) Ed25519 key pair, in the format `usign -G` writes.
// The feed key in feed.pub was made with it; the secret half is the OPKG_FEED_KEY CI secret.
//
// usage: node usign-keygen.mjs <secret key out> <public key out>
import { createHash, createPrivateKey, createPublicKey, randomBytes } from 'node:crypto';
import { writeFileSync } from 'node:fs';

const [secretOut, publicOut] = process.argv.slice(2);
const seed = randomBytes(32);
// usign clamps the seed it stores; harmless, but match it
seed[0] &= 0xf8; seed[31] &= 0x7f; seed[31] |= 0x40;

const pkcs8 = Buffer.concat([Buffer.from('302e020100300506032b657004220420', 'hex'), seed]);
const pub = Buffer.from(createPublicKey(createPrivateKey({ key: pkcs8, format: 'der', type: 'pkcs8' }))
  .export({ format: 'jwk' }).x, 'base64url');

const fingerprint = randomBytes(8);
const seckey = Buffer.concat([seed, pub]);
const checksum = createHash('sha512').update(seckey).digest().subarray(0, 8);
const kdfrounds = Buffer.alloc(4);
const secret = Buffer.concat([Buffer.from('EdBK'), kdfrounds, randomBytes(16), checksum, fingerprint, seckey]);
const pubkey = Buffer.concat([Buffer.from('Ed'), fingerprint, pub]);
const fp = fingerprint.toString('hex');

writeFileSync(secretOut, `untrusted comment: private key ${fp}\n${secret.toString('base64')}\n`, { mode: 0o600 });
writeFileSync(publicOut, `untrusted comment: cpxy opkg feed ${fp}\n${pubkey.toString('base64')}\n`);
console.log(fp);
