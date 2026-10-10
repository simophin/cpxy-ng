# Setting up a cpxy-gateway exit node on Debian

A start-to-finish walkthrough: a fresh Debian box (bare metal or a VM) becomes a Tailscale exit node
whose clients' traffic goes through a cpxy server. For every setting and for troubleshooting, see the
[cpxy-gateway user guide](cpxy-gateway.md).

You need:

- A Debian 12 or 13 (or Ubuntu 24.04) machine, `amd64` or `arm64`, with root access.
- A Tailscale account with admin rights on the tailnet, to approve the exit node.
- A running cpxy server, and its URL with the key: `https://:<key>@<host>:<port>`.

## 1. Prepare the machine

Install Debian as usual; a minimal install with SSH is enough. A small VM will do: 1 vCPU, 512 MB
of RAM and 4 GB of disk.

**In a VM, give it its own address on the LAN** rather than putting it behind the hypervisor's NAT.
The fewer NAT layers in front of the box, the more often Tailscale connects clients directly instead
of relaying. With libvirt, the simplest way is macvtap, which needs no bridge on the host:

```xml
<interface type='direct'>
  <source dev='enp0s31f6' mode='bridge'/>   <!-- the host's wired NIC -->
  <model type='virtio'/>
</interface>
```

The VM then gets its own DHCP lease and IPv6 addresses from the router. The one catch: the host and
the VM cannot reach each other over the LAN, so use `virsh console` until Tailscale is up, then SSH
over Tailscale. Wi-Fi interfaces do not work for this; use a wired one.

Then update the system:

```sh
sudo apt update && sudo apt full-upgrade
```

## 2. Install Tailscale and advertise an exit node

```sh
curl -fsSL https://tailscale.com/install.sh | sh
sudo tailscale up --advertise-exit-node
```

Open the login link it prints and sign in. It may warn that IPv6 forwarding is disabled; that is
expected and harmless here, because cpxy-gateway refuses clients' IPv6 anyway so they use IPv4. The
package turns on IPv4 forwarding in step 4.

In the [admin console](https://login.tailscale.com/admin/machines):

1. Find the machine, open its **⋯** menu, choose **Edit route settings** and tick **Use as exit node**.
2. In the same menu, choose **Disable key expiry**, so the box does not drop off the tailnet in a few
   months.
3. Under **DNS**, make sure no global nameserver has **Use with exit node** on. Otherwise clients send
   their DNS there instead of to this box, and miss the split DNS.

## 3. Download cpxy-gateway

Get `cpxy-gateway_<version>_<arch>.deb` from the
[GitHub releases](https://github.com/simophin/cpxy-ng/releases) (releases after 1.12.0 include it):

```sh
arch=$(dpkg --print-architecture)
version=<version>     # e.g. the latest release, without the leading "v"
curl -fLO "https://github.com/simophin/cpxy-ng/releases/download/v$version/cpxy-gateway_${version}_$arch.deb"
```

Until a release includes it, take it from the `debian-package-amd64` (or `-arm64`) artifact of the
latest CI run on `main`, e.g. with `gh run download -n debian-package-amd64 -R simophin/cpxy-ng`.

If GitHub is slow or blocked where the box is, download the file elsewhere and copy it over with
`scp`.

## 4. Install and configure

```sh
sudo apt install ./cpxy-gateway_*.deb
sudoedit /etc/cpxy/cpxy.conf
```

At minimum, set the server:

```sh
SERVER=https://:<key>@<host>:<port>
```

Then check the DNS servers:

- `DNS_UPSTREAM` (default `223.5.5.5,tcp://119.29.29.29`) is used for names that resolve to the
  local region. The resolvers your router hands out over DHCP are usually the fastest and give
  the nearest CDN nodes for your ISP: `resolvectl status` lists them, e.g.
  `DNS_UPSTREAM=192.168.1.1`.
- `DNS_ALTERNATIVE` (default Google over HTTPS) is used for everything else. It must not be poisoned
  on its way to the box: plain DNS to an overseas server (such as `1.1.1.1`) is, and DNS over HTTPS
  works only while its server is not blocked. The reliable choice is a resolver on your tailnet,
  set up in the next step.

Leave the other settings alone unless the [user guide](cpxy-gateway.md#settings) says otherwise for
your setup.

## 4b. Recommended: an overseas resolver on your tailnet

Queries to a tailnet machine travel inside Tailscale's encrypted tunnel, so they cannot be poisoned
or blocked by name. Pick a Debian machine outside the local region that is on the tailnet, ideally the cpxy
server itself, so CDNs answer for where traffic leaves. On it:

```sh
sudo apt install dnsmasq-base       # the binary only; the full dnsmasq package would want port 53
sudo mkdir -p /etc/tailnet-dns
sudo tee /etc/tailnet-dns/dnsmasq.conf <<'EOF'
port=5335
interface=tailscale0
bind-dynamic
no-resolv
no-hosts
server=1.1.1.1
server=8.8.8.8
cache-size=10000
EOF
sudo tee /etc/systemd/system/tailnet-dns.service <<'EOF'
[Unit]
Description=dnsmasq resolver for tailnet peers on port 5335
After=network-online.target tailscaled.service
Wants=network-online.target

[Service]
ExecStart=/usr/sbin/dnsmasq --keep-in-foreground --conf-file=/etc/tailnet-dns/dnsmasq.conf --pid-file=
DynamicUser=yes
Restart=on-failure

[Install]
WantedBy=multi-user.target
EOF
sudo systemctl daemon-reload
sudo systemctl enable --now tailnet-dns
```

It answers only on the machine's Tailscale addresses. Any port works; 5335 keeps it clear of
another resolver on 53. If the machine runs a firewall, allow port 5335 from `tailscale0`, and if
your tailnet has access rules, allow the gateway to reach it.

Then point the gateway at it, using the machine's Tailscale IP (`tailscale ip -4 <machine>`), in
`/etc/cpxy/cpxy.conf`:

```sh
DNS_ALTERNATIVE=100.x.y.z:5335
```

## 5. Start it

```sh
sudo systemctl enable --now cpxy-dns-split cpxy-router
```

Both start on every boot from now on. Check that they run and that the box's resolver changed:

```sh
systemctl status cpxy-router cpxy-dns-split
cat /etc/resolv.conf                       # first line: "Managed by cpxy-dns-split"
journalctl -u cpxy-router -u cpxy-dns-split -f
```

`cpxy-dns-split` turns off Tailscale's "accept DNS" on this box and logs that it did; that is
intended (see [DNS](cpxy-gateway.md#dns)).

## 6. Test from a client

On a phone, choose the box under **Exit node** in the Tailscale app. On a Linux or macOS client:

```sh
tailscale set --exit-node=<box name>
curl https://ifconfig.me          # should print the cpxy server's public IP
```

Then open a local-region site (one hosted in the box's region): it should load directly, and
`journalctl -u cpxy-router -f` on the box shows each connection and whether it was proxied. Stop
using the exit node with `tailscale set --exit-node=`.

## Afterwards

- **Change settings:** edit `/etc/cpxy/cpxy.conf`, then
  `sudo systemctl restart cpxy-router cpxy-dns-split`.
- **Upgrade:** download the newer `.deb` and `sudo apt install ./cpxy-gateway_<newer>.deb`. Settings
  are kept and running services restart on the new version.
- **Pause:** `sudo systemctl stop cpxy-router cpxy-dns-split` sends exit-node traffic out directly
  again; `start` brings cpxy back.
- **Remove:** `sudo apt purge cpxy-gateway` restores the original routing and DNS. To stop being an
  exit node too, run `sudo tailscale set --advertise-exit-node=false`.
