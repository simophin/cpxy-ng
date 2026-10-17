# CPXY VPN app

The Android (and later iOS) VPN app that runs the Rust `mobile-engine` on the device. See
[docs/mobile-vpn-plan.md](../docs/mobile-vpn-plan.md) for what it does and how.

Run Gradle from this directory and Cargo commands from the repository root.

- `shared/`: Compose Multiplatform UI, profile storage and the `VpnController` interface.
- `androidApp/`: `CpxyVpnService` (the `VpnService` running the engine), `AndroidVpnController`
  and the activity. Application ID `dev.fanchao.cpxy.vpn`.

## Build and test

```bash
./gradlew :shared:allTests
./gradlew :androidApp:assembleDebug
./gradlew :androidApp:assembleRelease
```

The release variant uses the checked-in debug keystore.

The APK tasks build `libmobile_engine.so` for the four Android ABIs with cargo-ndk, and generate
the UniFFI Kotlin bindings from a host debug build of the engine (release libraries are stripped
of the UniFFI metadata). Both are declared task outputs under `androidApp/build/generated/`.
Install the toolchain once:

```bash
rustup target add \
  aarch64-linux-android \
  armv7-linux-androideabi \
  x86_64-linux-android \
  i686-linux-android

cargo install cargo-ndk --version 4.1.2 --locked
sdkmanager "ndk;28.2.13676358"
```

## CI and release APK

The repository CI runs the engine unit tests, the Linux namespace TUN lab and
`:shared:allTests` on pull requests, main and tags. On main and tags it also builds the
release APK with the pinned NDK and cargo-ndk, then checks that `libmobile_engine.so` is packaged
for `arm64-v8a`, `armeabi-v7a`, `x86` and `x86_64`, and that it is signed with the release key.

Download it from the [download server](../docs/downloads.md) at
`https://<download host>/latest/android/cpxy-vpn.apk`, or the `android` artifact of a CI run. CI
sets the version (`CPXY_VPN_VERSION`) to `git describe`, such as `1.15.0-3-gabc1234`; local builds
are `dev`. The Settings tab shows it. Installed from CI, the app updates itself from the download
server (`CPXY_UPDATE_URL`).

Release APKs are signed with `mobile/release.keystore` when `ANDROID_KEYSTORE_PASSWORD` is set (CI
has it as a secret), and with the debug key otherwise. The two cannot update each other.

## Manual smoke check

1. Install the debug APK, add a profile (server URL with key, upstream and alternative DNS) and
   connect; accept the VPN and notification prompts.
2. Browse a CN site and a non-CN site. The home screen lists each connection as `direct` or
   `proxy`.
3. Check the public IP (e.g. `https://ifconfig.me`): it is the cpxy server's.
4. Disconnect from the app or the notification.
5. In Settings, add the quick settings tile; it connects and disconnects with the selected
   profile.
6. In Settings, choose "Only selected apps" with a browser, reconnect, and check that only the
   browser goes through the VPN (`adb shell dumpsys connectivity | grep 'VPN CONNECTED'` lists
   its UID ranges).
7. Turn on always-on VPN from Settings; the VPN connects with the selected profile.

The engine logs to logcat with the tag `cpxy-engine`.

## Share the VPN connection over Wi-Fi

In Settings → Share connection, enable HTTP proxy and/or SOCKS5 proxy and choose their ports
(defaults: HTTP 8080, SOCKS5 1080). Both are off by default. Connect or reconnect the VPN to
apply changes, then configure another device with the Android device's Wi-Fi IPv4 address as
its proxy host and the selected port. HTTP supports ordinary requests and HTTPS CONNECT;
SOCKS5 supports TCP CONNECT, including hostname requests (no UDP ASSOCIATE).

The listeners bind to `0.0.0.0` on all IPv4 interfaces and have no authentication. They run
only while the VPN engine runs. Proxy hostnames use the profile's DNS split,
and destinations use the engine's direct/CN versus cpxy routing. App selection affects local
Android apps; shared proxy connections use the engine regardless of the selected apps.
If an enabled port is occupied or cannot be bound, the VPN connection fails with the bind
error. Enabled HTTP and SOCKS5 listeners must use different ports.

For a smoke check, enable both proxies and reconnect. From another device on the same Wi-Fi,
run (replacing `PHONE_IP`):

```bash
curl --proxy http://PHONE_IP:8080 https://ifconfig.me
curl --proxy socks5h://PHONE_IP:1080 https://ifconfig.me
```

Disconnect and confirm both proxies stop accepting connections. Then disable one proxy,
reconnect, and confirm only the enabled proxy accepts connections.

## Fixed proxy routes

Settings → Routing → Proxy routes edits IPv4 and IPv6 CIDRs, one per line. Matching TCP
destination IPs use the main proxy even when automatic routing would send them
direct. Hostname requests from shared HTTP/SOCKS listeners are matched after
resolution. The list applies to every profile; save and reconnect to apply it.
Clear the editor and save to remove all fixed routes. Invalid entries cannot be
saved.

## IPv6

Each profile chooses its IPv6, since it depends on the server:

- **Full** (the default): IPv6 goes direct or through the server like IPv4. The server must reach
  IPv6, as it connects to the IPv6 addresses itself.
- **Direct only**: IPv6 that goes direct (CN and private addresses) is relayed; the rest is
  refused before the app's connection opens, so the app falls back to IPv4 at once, and DNS gives
  out only the AAAA records that go direct. For a server without IPv6.
- **Off**: the VPN interface has no IPv6 address, so apps use IPv4 only.

Unless it is off, the interface gets the unique-local `fd00:a:e9::1`, so apps use IPv6 for names
without IPv4 and prefer IPv4 otherwise. On a network without IPv6, IPv6 that would go direct is
refused the same way. The traffic log has no flags for IPv6 addresses.

## Android TV and keyboard smoke check

The same APK appears in both phone and Android TV launchers. TV devices do not need a
touchscreen; the TV launcher uses the CPXY VPN banner.

- On TV, press Up from the tab bar or the Home + button to enter the current screen.
  On Home, use Left/Right between a profile and its hamburger menu. In Settings, use
  Left/Right between an app filter and its Choose button; Center activates the focused
  control. Navigate through proxy routes, proxy switches and port dialogs with Up/Down.
- Choose either app filter. Press Down from the top bar to the search box and on to
  the apps; focusing the search box must not open the keyboard. Center opens it, Back
  closes it, and Down continues to the filtered apps.
- Open a profile's hamburger menu and choose Edit or Duplicate. Duplicate opens a new
  profile prefilled with the server and DNS lists; Save creates and selects the copy.
  Back discards the draft without changing the original.
- On a phone, use the keyboard's Next action through Name, Server and Upstream DNS.
  Done on Alternative DNS validates and saves, just like Save. DNS fields still allow
  pasted lists with one server per line; long lists scroll inside the field.
- With the keyboard open, type at the bottom of a long DNS list. The form should stay
  above the keyboard without jumping, and Save should remain reachable by scrolling.

UI regression tests run on a connected phone or TV emulator:

```bash
./gradlew :androidApp:connectedDebugAndroidTest
```

## DNS search domains

Settings → DNS → DNS search domains accepts one ASCII hostname suffix per line, such as
`home.arpa` and `corp.example`. The ordered list applies to every profile after reconnecting;
clear it and save to disable it. Android receives the list for its VPN resolver, and the engine
also expands single-label queries from VPN traffic and shared HTTP/SOCKS proxy clients.

For `printer`, the engine tries `printer.home.arpa`, then `printer.corp.example`, then the bare
name, stopping at the first successful answer with records. The engine queries names containing
dots unchanged; Android also applies its own resolver search rules. Expanded names use the
usual DNS split and cache. AAAA queries are answered according to the profile's
IPv6 (see [IPv6](#ipv6)).

### Search-domain emulator smoke check

Use a reachable DNS server with test A records such as `printer.home.arpa` and
`second.corp.example`, both pointing to a reachable HTTP test endpoint. Configure that DNS
server as the profile's upstream and alternative resolver.

1. In Settings → DNS → DNS search domains, enter `home.arpa` and `corp.example`, one per line.
   An invalid suffix such as `a..` must show an error and disable Save. Save the valid list,
   restart the app, and confirm the list remains in the same order.
2. Connect the VPN. In an Android app, open `http://printer:PORT/` and
   `http://second:PORT/`. Both should reach the test endpoint. With A records for the same
   short name in both domains, the first domain should win.
3. Check an existing fully qualified hostname, a bare name known by the DNS server, and an
   unknown name. They should resolve unchanged, fall back to the bare name, and fail,
   respectively. Direct UDP and TCP queries to the VPN DNS server (`10.233.0.2:53`) should
   preserve the original question and link successful expanded names with a CNAME.
4. Enable the shared HTTP/SOCKS proxies, reconnect, and check the same names from a second
   device using the proxies. Hostnames must be resolved by the proxy device.
5. Clear the search-domain list and save. The active VPN keeps its previous configuration
   until reconnection. After reconnecting, short names that require a suffix must fail;
   fully qualified names and names the DNS server knows without a suffix must still work.
   Android's resolver can cache earlier results, so use a fresh test name when checking
   changes, or send a direct DNS query to the VPN DNS server.
