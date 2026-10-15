# Downloads and publishing

Every push to `main` builds everything and publishes it to the download server: the Android app,
the OpenWrt package and the plain binaries. There are no GitHub releases. Docker images go to GHCR
as before.

The server is a Cloudflare Worker ([download-worker/](../download-worker)) in front of an R2 bucket.
Its address is the `DOWNLOAD_URL` repository variable, written `https://<download host>` below.

## Layout

One folder per version, and every folder names its files the same:

```
<version>/
  SHA256SUMS                                 checksums of everything below; uploaded last
  android/cpxy-vpn.apk
  openwrt/<opkg arch>/cpxy.ipk               aarch64_cortex-a53, x86_64
  openwrt/<opkg arch>/Packages{,.gz,.sig}    a signed opkg feed
  raw/<rust target>/cpxy-client              also cpxy-server, cpxy-dns-split (.exe on Windows)
  raw/<rust target>/cpxy-tsgw                the two Linux musl targets only
```

The Rust targets are `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`,
`x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc` and
`i686-pc-windows-msvc`.

The version is `git describe`: a tag such as `1.17.0` on the tagged commit, and
`1.17.0-5-gabc1234` five commits after it. Versions are ordered by the tag's numbers, then the
commit count, so later commits on `main` are newer.

- `https://<download host>/` lists the versions, newest first.
- `https://<download host>/latest` is the newest complete version's name (it has its
  `SHA256SUMS`).
- `https://<download host>/latest/<path>` redirects to `<path>` in that version, for example
  `/latest/raw/x86_64-unknown-linux-musl/cpxy-server` or `/latest/android/cpxy-vpn.apk`.

## Updates

- **Android:** the app checks `latest/android/cpxy-vpn.apk` once a day, and from Settings → Check
  for updates. It reads the version from the redirect, downloads the APK from the version folder,
  checks it against `SHA256SUMS` and hands it to the system installer. The VPN reconnects after
  the update. The first update asks the user to allow installing apps from CPXY VPN. Builds
  without `DOWNLOAD_URL`, such as local ones (version `dev`), do not check.
- **OpenWrt:** the package adds the feed `https://<download host>/latest/openwrt/<arch>` and its
  signing key, so after the first install `opkg update && opkg upgrade cpxy` installs the newest
  version. The package version is the build's version with the commit count as an opkg revision:
  `1.17.0-r5`, and `1.17.0-r0` for the tag.

## Making a new version number

Tag a commit on `main` and push the tag:

```sh
git tag 1.18.0 && git push origin 1.18.0
```

The tag's build is published as `1.18.0`, and later commits as `1.18.0-<n>-g<hash>`.

## Signing keys

- **Android:** `mobile/release.keystore`, alias `cpxy-vpn`, checked in. Its password (store and
  key) is the `ANDROID_KEYSTORE_PASSWORD` secret; Gradle reads it from the environment. CI checks
  the APK carries this certificate (SHA-256 `8b:a8:b5:74:…:67:e2:5b`). An update only installs over
  an app signed with the same key, so a new key means uninstalling the app once. Local release
  builds without the password are signed with `mobile/debug.keystore`.
- **opkg feed:** a usign key. The public half is `packaging/openwrt/feed.pub` (fingerprint
  `5dd26ca9aa5c5dbc`), which the package installs in `/etc/opkg/keys`; the secret half is the
  `OPKG_FEED_KEY` secret. `packaging/openwrt/usign-keygen.mjs` makes a new pair; routers then need
  a package with the new public key installed by hand once.

## Setting up the server

1. In Cloudflare, create an R2 bucket, and an R2 API token with Object Read & Write on it.
2. Create an API token for the Worker: Workers Scripts Edit, Workers Routes Edit, and Zone Read
   for the zone of the download host.
3. Set the repository secrets `R2_ACCESS_KEY_ID`, `R2_SECRET_ACCESS_KEY` and
   `CLOUDFLARE_API_TOKEN`, and the variables `CLOUDFLARE_ACCOUNT_ID`, `R2_BUCKET` and
   `DOWNLOAD_URL` (`https://<download host>`).
4. Run the `Download worker` workflow. It deploys the Worker on the download host as a custom
   domain.
5. Push to `main`, or run CI on `main`. The `publish` job uploads the build, then fetches its
   `SHA256SUMS` back through the server.

Until `R2_BUCKET` is set, `publish` is skipped and the build is only in the CI run's artifacts.

Nothing is deleted automatically. Each version holds every binary for every platform, so delete
old version folders in the R2 dashboard when the bucket grows. Avoid an age-based lifecycle rule: after a quiet spell it would
delete the latest version too.
