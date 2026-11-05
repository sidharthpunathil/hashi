# Remote access and HTTPS

Hashi listens on `127.0.0.1` over plain HTTP by default, which is fine for apps on the same machine. To reach it from elsewhere, pick one of these. Each gives you HTTPS.

| Option | Command | Address | Good for |
|---|---|---|---|
| Cloudflare quick tunnel | press `t` in the dashboard, or `hashi serve --tunnel` | random `https://*.trycloudflare.com`, new on every start | trying it out, short sessions |
| Cloudflare named tunnel | `hashi serve --tunnel-token <token> --public-url https://hashi.example.com` | your own hostname, stable | sharing invite links that last |
| Tailscale Serve | `hashi serve --tailscale serve`, or the Commands view (`5`) | `https://<machine>.<tailnet>.ts.net`, stable | people on your tailnet |
| Tailscale Funnel | `hashi serve --tailscale funnel`, or the Commands view (`5`) | `https://<machine>.<tailnet>.ts.net`, stable | anyone, with no Cloudflare account |
| Hashi serves HTTPS itself | `hashi serve --tls-cert cert.pem --tls-key key.pem` | wherever you point DNS or your VPN | LANs, Tailscale, your own certificates |
| Your reverse proxy | run Caddy or nginx in front, then `--public-url https://... --trust-proxy` | your proxy's address | existing setups |

## Quick tunnel

Install `cloudflared` (or point `--cloudflared-bin` / `HASHI_CLOUDFLARED_BIN` at one): `hashi install cloudflared` shows Cloudflare's documented command for your platform (see [installing the helpers](#installing-the-helpers)), or use [Cloudflare's downloads page](https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/). Then either press `t` in the dashboard of a running Hashi, or start with `--tunnel`. Both run the same tunnel: the header shows "starting tunnel" and then the URL, and invite links use it unless `--public-url` is set. If `cloudflared` is missing, pressing `t` opens a popup with the install command. To stop the tunnel without quitting, use "Stop going public" in the Commands view (`5`).

An invite popup that is open when the tunnel comes up switches to the public link, and `c` copies whichever link is shown. While Hashi has a public URL it writes it to `~/.hashi/public-url` (readable only by you) and removes it on shutdown, so `hashi share` in another terminal prints a public link too.

Hashi trusts the `CF-Connecting-IP` header from loopback only while a tunnel is configured or running, so per-client rate limits see the real visitor. Hashi accepts only a URL of the form `https://<label>.trycloudflare.com` from `cloudflared`. Because the URL changes on every start, invite links made against it stop working after a restart.

## Tailscale Serve and Funnel

If you already use [Tailscale](https://tailscale.com), Hashi can put itself behind your machine's stable `https://<machine>.<tailnet>.ts.net` name, with no Cloudflare account and no certificate to manage.

| Mode | Start it | What Hashi runs | Who can reach Hashi |
|---|---|---|---|
| Serve | `hashi serve --tailscale serve`, or Commands view (`5`) | `tailscale serve --bg --https=443 http://127.0.0.1:7878` | devices on your tailnet, and people you share the machine with |
| Funnel | `hashi serve --tailscale funnel`, or Commands view (`5`) | `tailscale funnel --bg --https=443 http://127.0.0.1:7878` | anyone on the internet |

`--tailscale-port` (`HASHI_TAILSCALE_PORT`) picks the HTTPS port on the tailnet side; it defaults to 443, and Funnel only accepts 443, 8443 or 10000. `--tailscale-bin` (`HASHI_TAILSCALE_BIN`) points at a `tailscale` that is not on `PATH`. `HASHI_TAILSCALE=serve|funnel` does the same as the flag. `--tailscale` cannot be combined with `--tunnel` or `--tunnel-token`.

**Requirements.** The `tailscale` CLI, installed and signed in (`hashi install tailscale` shows Tailscale's own install command; sign in with `sudo tailscale up` on Linux, or from the Tailscale app on macOS and Windows). Hashi listens on `127.0.0.1` or `0.0.0.0` over plain HTTP, because Tailscale proxies only to `http://127.0.0.1`; with `--tls-cert` or another `--bind`, Hashi refuses and says why. Funnel also needs MagicDNS, HTTPS certificates and the `funnel` node attribute in your tailnet policy ([Tailscale's Funnel requirements](https://tailscale.com/kb/1223/funnel)). When Funnel is not yet allowed, the Tailscale CLI prints a link to approve it: Hashi shows that link, made terminal safe, in a popup in the dashboard or as a log line in headless mode. Only `https://login.tailscale.com/...` links are shown, or links on the host in `HASHI_TAILSCALE_CONTROL_HOST` for a self-hosted control server such as Headscale. Hashi waits up to 60 seconds before giving up so you can try again.

**Sharing.** Serve is private to your tailnet. A friend can open an invite link served this way only if they are on your tailnet, or you share the machine with their Tailscale account from the Tailscale admin console. For people without Tailscale, use Funnel or a Cloudflare tunnel. Tailscale adds identity headers to Serve requests, but Hashi ignores them: every request still needs a Hashi token.

**What Hashi turns on and off.** Before starting, Hashi checks `tailscale status --json` (signed in, and the device's DNS name) and `tailscale serve status --json`. If the port already has a listener Hashi did not set up, it refuses and suggests another `--tailscale-port`; a listener that is exactly Hashi's own proxy, left by a run that did not stop cleanly, is reused. While the route is up:

- `https://<machine>.<tailnet>.ts.net` is the base for invite links (a configured `--public-url` still wins) and is written to `~/.hashi/public-url`.
- That host name is accepted by the `Host` check.
- `X-Forwarded-For` from loopback is trusted for rate limits, because Tailscale sets it.

All three end when the route stops. On stop (Commands view, "Stop going public") and on shutdown, Hashi runs only the matching off command, such as `tailscale serve --https=443 off`, for the listener it turned on. It never runs `tailscale serve reset` or `funnel reset`, which would delete the rest of your Serve config. Every 30 seconds Hashi checks the listener is still there; if something else removed it, the dashboard says so and Hashi leaves that port alone.

## One public route at a time

The Cloudflare quick tunnel and Tailscale are alternatives: only one runs at a time. Starting one while the other is up asks whether to switch, and switching stops the first before starting the second. "Stop going public" in the Commands view stops whichever is running, including a tunnel started with `--tunnel`, and links go back to local.

## Installing the helpers

`hashi install cloudflared` and `hashi install tailscale` print the vendor's own install command for your platform and run it only after you answer yes (`--yes` skips the question). Hashi never downloads and runs a binary itself.

| Helper | macOS | Linux | Windows |
|---|---|---|---|
| cloudflared | `brew install cloudflared` ([Cloudflare downloads](https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/)) | Cloudflare's package repository with `apt-get` or `yum` ([pkg.cloudflare.com](https://pkg.cloudflare.com/)) | the downloads page: Cloudflare documents no winget package |
| tailscale | with Homebrew, `brew install --formula tailscale` and `sudo brew services start tailscale` ([Tailscale's open source variant](https://github.com/tailscale/tailscale/wiki/Tailscaled-on-macOS)); otherwise the [recommended app](https://tailscale.com/download/mac) | `curl -fsSL https://tailscale.com/install.sh \| sh` ([install-linux](https://tailscale.com/kb/1031/install-linux)) | the [download page](https://tailscale.com/download/windows): Tailscale documents no winget package |

Package managers are looked up on `PATH` only. Without one the docs name, Hashi prints the download page instead. In the dashboard, a missing helper opens the same command in a popup: `enter` runs it in your terminal (the dashboard steps aside so `sudo` can ask for your password, then comes back), `c` copies it and `esc` cancels. `hashi doctor` reports both helpers and whether Tailscale is signed in.

## Named tunnel

In the Cloudflare dashboard, create a tunnel under Zero Trust, add a public hostname pointing at `http://localhost:7878` (or `https://localhost:7878` with "No TLS Verify" when Hashi serves TLS itself), and copy the tunnel token. Put it in `HASHI_TUNNEL_TOKEN` rather than your shell history. Hashi passes it to cloudflared through an environment variable, so it never shows up in the process list.

Hashi always gives cloudflared its own empty config file, so an existing `~/.cloudflared/config.yml` from another project cannot redirect Hashi's tunnel. Hashi never downloads `cloudflared` for you, because running an unverified binary fetched at runtime is a supply-chain risk.

## Native HTTPS

Give Hashi a PEM certificate chain and key. Hashi speaks TLS 1.2 and 1.3 through rustls and sends `Strict-Transport-Security`.

- Your own domain: get a certificate from Let's Encrypt (for example `certbot certonly --standalone -d hashi.example.com`).
- A Tailscale network: `tailscale cert <machine>.<tailnet>.ts.net` gives a trusted certificate for your machine's tailnet name. Bind to the tailnet address with `--bind`.
- A home network: `mkcert` creates a local CA your own devices can trust.

Then set `--public-url` to the address people will use, so invite links point at the right place.

## Behind a reverse proxy

Pass `--trust-proxy` so Hashi reads each client's address from `X-Forwarded-For` (only on connections from loopback) and rate limits people separately instead of as one.

Over plain HTTP, Hashi answers only requests whose `Host` is an IP address, `localhost`, the `--public-url` host or a name you list with `--allowed-host`. Caddy forwards the original host name, so set `--public-url` or `--allowed-host` for it. Web pages on other sites cannot call Hashi unless you list their origin with `--cors-origin`.
