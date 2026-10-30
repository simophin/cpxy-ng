# Router traffic API and browser view

With OpenWrt's opt-in `router_geoip` mode, regional/private IPv4 flows are forwarded by the
kernel and do not appear in this API or its counters. The API covers traffic entering the packet
engine. Use `cpxy-show` for kernel-route generation, count and dataset status.

`cpxy-router` can serve a read-only traffic API and a small browser page from the same listener.
The page's HTML, CSS and JavaScript are embedded in the binary: no npm build, CDN, external
fonts, analytics, or internet connection is needed to load it. It uses a small local stylesheet
and vanilla JavaScript. The API works independently of the page.

## Enable it

For a standalone Linux worker, add `--api-listen` to its existing arguments:

```sh
cpxy-router --socks5-proxy 127.0.0.1:1080 --tun cpxy0 --api-listen 127.0.0.1:3010
```

`API_LISTEN=127.0.0.1:3010` is the equivalent environment setting. The listener is disabled
when neither is set. IPv4 and bracketed IPv6 socket addresses are accepted. An occupied API
port fails startup before the TUN worker starts. Stopping the worker also closes the listener.

On OpenWrt, enable it for an instance:

```sh
uci set cpxy.main.router_api='1'
uci commit cpxy
/etc/init.d/cpxy-router reload
```

It listens on `127.0.0.1:3010+index`: main at 3010, guest with index 1 at 3011, etc.
`router_api` may be inherited from `defaults`; `router_api_listen` is an instance-only setting.
`cpxy-show main` prints the effective address. As with the packet worker, the instance must have
`router=1` and at least one `network` to run. Reloading the worker clears its in-memory history
and byte totals.

From your computer, forward the port over SSH:

```sh
ssh -N -L 3010:127.0.0.1:3010 root@192.168.2.1
```

Open **http://127.0.0.1:3010/**. To access it directly on a trusted management network instead,
set an address belonging to that interface, e.g.:

```sh
uci set cpxy.main.router_api_listen='192.168.2.1:3010'
uci commit cpxy
/etc/init.d/cpxy-router reload
```

Then open **http://192.168.2.1:3010/**. Access follows your existing firewall input policy; the
package adds no firewall accept rule for this port. The listener has **no authentication or
TLS** and exposes destination history: keep it on loopback with SSH, a trusted management
interface, or behind an authenticated reverse proxy. Avoid wildcard/WAN listeners.

## Browser behavior and scope

The page shows destination names/IPs and ports, direct/proxy routing, connection time, latency,
and errors, plus total uploaded/downloaded IP bytes through the instance's TUN. It polls once
per second while visible. Following keeps the newest connections in view; scrolling away or
loading older entries pauses following. Use **Latest** to return. Hidden tabs stop polling.
Temporary failures retry automatically; a new worker session resets the displayed history.

The engine keeps its most recent 10,000 TCP connection attempts in RAM. The browser keeps a
window of at most 2,000 rows, with pages of 200. Older entries are evicted, and all history is
lost when the worker exits. These are the same records and pagination that Android uses.
Events are best effort: the engine can drop notifications if its event listener falls behind.

This is a connection log, not a list of currently open sockets or a per-device bandwidth
accounting service. There are no per-connection byte totals, client/device identities, DNS/UDP
log entries or country flags. Country flags would require shipping the full country database;
the router currently embeds only local-region routing data. Router-originated traffic and
connections through the separate `cpxy-client` and `cpxy-tsgw` services are outside this worker's
view. Byte counters measure TUN IP bytes, not WAN/proxy transport bytes, and exclude the
router's separately served DNS.

## HTTP API

All endpoints below use GET and return JSON. Responses are not cached; CORS is not enabled.
Connection events have the same fields as the Rust/Android engine records, using snake_case.
Unknown query parameters, invalid integers, contradictory cursors, and limits outside 1–1000
return HTTP 400.

| Request | Result |
|---|---|
| `/api/traffic` | `{ "session": "…", "running": true, "sent": 123, "received": 456 }` |
| `/api/connections?limit=200` | The newest page, in oldest-first order. |
| `/api/connections?since=123&limit=200` | The oldest page with `seq >= 123`, in oldest-first order. |
| `/api/connections?before=123&limit=200` | The newest page with `seq < 123`, in oldest-first order. |

`limit` defaults to 200. Supply at most one of `since` and `before`. A connection response is:

```json
{
  "session": "e18d99aeec5a4b8eb311d8ecf0aaf842",
  "oldest_seq": 100,
  "records": [
    {
      "seq": 123,
      "event": {
        "host": "example.com",
        "ip": "93.184.216.34",
        "port": 443,
        "outbound": "proxy",
        "delay_millis": 42,
        "time_millis": 1792368000000,
        "error": null
      }
    }
  ]
}
```

`ip` can be null when unknown; `host` may be an IP when no name was observed. `error` is null
on success and a string on connection failure. Times are Unix epoch milliseconds; delay is
the time spent connecting, not a continuously measured ping.

Use `since=last_seq+1` to follow. Sequence numbers increase within the process and may have
gaps; `oldest_seq` identifies the oldest retained record, or the next sequence when empty.
If `oldest_seq > since`, reload the latest page because records have expired. When paging
backward, stop once the first record's sequence equals `oldest_seq`, or a page is empty.

Every response includes an opaque `session` identifier, stable for the listener's lifetime.
If it changes, discard old cursors and reload: a new worker process can reuse sequence numbers.
Do not persist a cursor without its session. This paged API is separate from `client_cn`'s
existing live-only `/events` WebSocket, which remains unchanged.

For example, through the SSH tunnel:

```sh
curl http://127.0.0.1:3010/api/traffic
curl 'http://127.0.0.1:3010/api/connections?limit=20'
```

## Development checks

`cargo test --locked -p mobile-engine` covers the shared history, HTTP contract and CLI.
On Linux, the routing lab (`packaging/openwrt/test/lab.sh`) also checks the API against real
TUN traffic and worker restarts; `packaging/openwrt/test/init-dryrun.sh` checks OpenWrt settings.

The browser regression test uses Node 22 or newer and an installed Chrome, with no npm packages:

```sh
node --test mobile-engine/test/traffic-ui.mjs
# If Chrome is not in its default location:
CHROME_BIN=/path/to/chrome node --test mobile-engine/test/traffic-ui.mjs
```

It starts a loopback fixture server and an isolated headless browser, then checks following,
pagination, history limits, eviction/restart recovery, retries, hidden tabs, safe text rendering,
and phone layout. CI runs it alongside the engine tests. Test fixtures are not bundled into
the router or served by its API.
