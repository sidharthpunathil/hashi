# Remote access and HTTPS

Hashi listens on `127.0.0.1` over plain HTTP by default, which is fine for apps on the same machine. To reach it from elsewhere, pick one of these. Each gives you HTTPS.

| Option | Command | Address | Good for |
|---|---|---|---|
| Cloudflare quick tunnel | `hashi serve --tunnel` | random `https://*.trycloudflare.com`, new on every start | trying it out, short sessions |
| Cloudflare named tunnel | `hashi serve --tunnel-token <token> --public-url https://hashi.example.com` | your own hostname, stable | sharing invite links that last |
| Hashi serves HTTPS itself | `hashi serve --tls-cert cert.pem --tls-key key.pem` | wherever you point DNS or your VPN | LANs, Tailscale, your own certificates |
| Your reverse proxy | run Caddy or nginx in front, then `--public-url https://... --trust-proxy` | your proxy's address | existing setups |

## Quick tunnel

Install `cloudflared` (`brew install cloudflared` or your platform's package) and add `--tunnel`. The URL appears in the dashboard header. Because it changes on restart, invite links made against it stop working after a restart.

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
