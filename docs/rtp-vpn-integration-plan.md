# RTP VPN replacement and OpenWrt integration

Status: implementation in progress in PR #153. The former RTP VPN has not been
deployed, so replace its mobile/worker integration with HTTP-controlled v2.
Keep the existing `cpxy-router` binary, service, configuration and behavior while
adding an opt-in RTP router service/package beside it.

- [x] Share the production HTTP/UDP client runtime between Linux and mobile.
- [x] Replace the mobile v1 facade with v2, protect TCP and UDP before connect,
  persist client identity, and obtain the inner address before establishing TUN.
- [x] Remove obsolete RTP bootstrap/worker interfaces and replace manual mobile address entry.
- [x] Finish replacement mobile documentation and Android build validation.
- [ ] Package the new RTP transport and an opt-in OpenWrt router service without
  taking over the existing cpxy-router configuration or selected networks.
- [ ] Preserve scoped routing guards, selected-network isolation, DNS policy and
  IPv6 refusal; validate configuration and resource ownership before mutation.
- [ ] Add QEMU OpenWrt fixtures using disposable disks and isolated networks.
  Verify TCP/UDP, address assignment, recovery, reload/reboot, cleanup and
  coexistence with the retained router.
- [ ] Run Rust/mobile/package checks, document actual results and limitations,
  commit reviewable stages and update PR #153.

QEMU must use private network/mount namespaces and a disposable overlay/snapshot;
no public server deployment or mutation of a live router is required. Stage only
fixture assets, generate test secrets outside the checkout and reap all task
processes/resources. Do not report emulated tests as real-router hardware tests.
