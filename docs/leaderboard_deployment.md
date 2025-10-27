# Global Leaderboard Deployment Guide

This document walks through turning the bundled `leaderboard_server` binary into a public service that desktop clients can use out of the box. The goal is:

- run the server on a VPS that you control
- expose it over HTTPS with your own domain
- bake the service URL into released dashboard builds so players never have to configure anything

> ⚠️ **Unsigned Binary Warning**
>
> The distributed desktop binaries are currently unsigned, so Windows may show a SmartScreen warning the first time they’re launched. The project is open source—feel free to build from source if you want a copy signed with your own trust chain.

---

## 1. Prerequisites

- Ubuntu 22.04 (or similar) VPS with root access
- Registered domain name (e.g. `playtime.mydomain.com`)
- Reverse proxy with TLS (instructions cover Caddy and Nginx + Certbot)
- Rust toolchain on the VPS (`curl https://sh.rustup.rs -sSf | sh`)

---

## 2. Build & Install the Server

```bash
# Pull the source on the VPS
cd /opt
sudo git clone https://github.com/Sudo-Spectral/SC-Playtime-Tracker.git
sudo chown -R $USER:$USER SC-Playtime-Tracker
cd SC-Playtime-Tracker

# Build a release binary
cargo build --release --bin leaderboard_server

# Install to /usr/local/bin and create a data directory
sudo install -Dm755 target/release/leaderboard_server /usr/local/bin/leaderboard_server
sudo install -d -m 755 /var/lib/sc-playtime
sudo chown -R scplaytime:scplaytime /var/lib/sc-playtime  # optional service user (see below)
```

Create a dedicated service account (optional but recommended):

```bash
sudo useradd --system --home /var/lib/sc-playtime --shell /usr/sbin/nologin scplaytime
sudo chown -R scplaytime:scplaytime /var/lib/sc-playtime
```

---

## 3. Systemd Service

Create `/etc/systemd/system/leaderboard.service`:

```ini
[Unit]
Description=Star Citizen Playtime Leaderboard API
After=network-online.target
Wants=network-online.target

[Service]
User=scplaytime
Group=scplaytime
ExecStart=/usr/local/bin/leaderboard_server
WorkingDirectory=/var/lib/sc-playtime
Environment=LEADERBOARD_ADDR=127.0.0.1:8080
Environment=LEADERBOARD_STORE=/var/lib/sc-playtime/leaderboard.json
Restart=on-failure
RestartSec=3

[Install]
WantedBy=multi-user.target
```

Reload and start:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now leaderboard.service
sudo systemctl status leaderboard.service
```

The service now listens on `127.0.0.1:8080`. Next step is to expose it via HTTPS.

---

## 4. Reverse Proxy & TLS

### Option A: Caddy (automatic HTTPS)

```bash
sudo apt install -y caddy

# /etc/caddy/Caddyfile
playtime.mydomain.com {
    encode gzip
    reverse_proxy 127.0.0.1:8080
}
```

Reload Caddy:

```bash
sudo systemctl reload caddy
```

### Option B: Nginx + Certbot

```bash
sudo apt install -y nginx certbot python3-certbot-nginx
sudo certbot --nginx -d playtime.mydomain.com
```

Then configure `/etc/nginx/sites-available/playtime`:

```nginx
server {
    listen 80;
    server_name playtime.mydomain.com;
    return 301 https://$host$request_uri;
}

server {
    listen 443 ssl http2;
    server_name playtime.mydomain.com;

    ssl_certificate /etc/letsencrypt/live/playtime.mydomain.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/playtime.mydomain.com/privkey.pem;
    include /etc/letsencrypt/options-ssl-nginx.conf;

    location / {
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_pass http://127.0.0.1:8080;
    }
}
```

Enable the site and reload Nginx:

```bash
sudo ln -s /etc/nginx/sites-available/playtime /etc/nginx/sites-enabled/
sudo nginx -t
sudo systemctl reload nginx
```

---

## 5. Embed the Endpoint in Client Builds

You want desktop clients to ship with the production endpoint baked in. The leaderboard client checks:

1. the persisted endpoint configured in the dashboard settings
2. `PLAYTIME_LEADERBOARD_URL` (runtime override)
3. `LEADERBOARD_DEFAULT_URL` (compile-time constant)
4. local fallback file if none of the above are set

Users can change the endpoint at runtime from **Settings → Leaderboard Sync**. Leaving the field blank keeps the baked-in default or local fallback.

Build releases like this (replace the URL with your domain):

```bash
LEADERBOARD_DEFAULT_URL=https://playtime.mydomain.com cargo build --release --bin dashboard
```

After embedding the URL, end users never need to set an environment variable—the dashboard will automatically sync/fetch from your hosted service.

---

## 6. Verify the API

```bash
curl -X POST https://playtime.mydomain.com/submit \
  -H "Content-Type: application/json" \
  -d '{"username":"TestPilot","total_minutes":120}'

curl https://playtime.mydomain.com/top
```

The first call should return `204 No Content`; the second should list the leaderboard entries.

---

## 7. Optional Hardening

- Put the service behind Cloudflare or another WAF for rate limiting
- Add OIDC or API keys if you need write protection
- Configure monitoring (fail2ban/systemd watchdog/Prometheus exporter)

---

With these steps the leaderboard is globally accessible and the desktop app ships with the correct endpoint baked in. Users only need to provide a display name to appear on the board.
