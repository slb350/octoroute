# Deployment Guide

Homelab deployment guide for Octoroute.

---

## Table of Contents

1. [Overview](#overview)
2. [System Requirements](#system-requirements)
3. [Binary Deployment](#binary-deployment)
4. [Systemd Service](#systemd-service)
5. [Docker Deployment](#docker-deployment)
6. [Reverse Proxy](#reverse-proxy)
7. [Security Hardening](#security-hardening)
8. [Monitoring Setup](#monitoring-setup)

---

## Overview

Octoroute can be deployed in several ways for homelab use:

- **Binary deployment**: Direct execution of compiled binary
- **Systemd service**: Managed by systemd for automatic startup
- **Docker container**: Containerized deployment
- **Behind reverse proxy**: With nginx or Caddy for SSL/auth

---

## System Requirements

### Minimum Requirements

- **CPU**: 1 core
- **RAM**: 256 MB
- **Disk**: 50 MB for binary + logs
- **Network**: Access to model endpoints

### Recommended

- **CPU**: 2+ cores (for concurrent requests)
- **RAM**: 512 MB - 1 GB
- **Disk**: 1 GB (for logs, metrics)
- **Network**: Low latency to model endpoints (<10ms ideal)

### Supported Platforms

- **Linux**: x86_64, aarch64 (Raspberry Pi 4+)
- **macOS**: x86_64, aarch64 (Apple Silicon)
- **Windows**: x86_64 (via WSL or native)

---

## Binary Deployment

### Build from Source

```bash
# Clone repository
git clone https://github.com/slb350/octoroute.git
cd octoroute

# Build release binary
cargo build --release

# Binary location
ls -lh target/release/octoroute
```

### Install Binary

```bash
# Copy to system bin directory
sudo cp target/release/octoroute /usr/local/bin/

# Verify installation
octoroute --version
```

### Create Configuration

```bash
# Create config directory
sudo mkdir -p /etc/octoroute

# Copy example config
sudo cp config.toml /etc/octoroute/config.toml

# Edit configuration
sudo nano /etc/octoroute/config.toml
```

### Run Manually

```bash
# Run with config
OCTOROUTE_CONFIG=/etc/octoroute/config.toml octoroute

# Or from working directory with local config.toml
cd /etc/octoroute
octoroute
```

---

## Systemd Service

### Create Service File

```bash
sudo nano /etc/systemd/system/octoroute.service
```

**Service Configuration**:

```ini
[Unit]
Description=Octoroute - Multi-Model Router for Local LLMs
After=network.target
Wants=network-online.target

[Service]
Type=simple
User=octoroute
Group=octoroute
WorkingDirectory=/etc/octoroute
ExecStart=/usr/local/bin/octoroute
Environment="OCTOROUTE_CONFIG=/etc/octoroute/config.toml"
Environment="RUST_LOG=octoroute=info"

# Restart policy
Restart=on-failure
RestartSec=5s

# Resource limits
LimitNOFILE=65536
MemoryLimit=1G

# Security hardening
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/log/octoroute

[Install]
WantedBy=multi-user.target
```

### Create User and Directories

```bash
# Create service user
sudo useradd -r -s /bin/false octoroute

# Create log directory
sudo mkdir -p /var/log/octoroute
sudo chown octoroute:octoroute /var/log/octoroute

# Set config permissions
sudo chown octoroute:octoroute /etc/octoroute/config.toml
sudo chmod 640 /etc/octoroute/config.toml
```

### Manage Service

```bash
# Reload systemd
sudo systemctl daemon-reload

# Enable service (start on boot)
sudo systemctl enable octoroute

# Start service
sudo systemctl start octoroute

# Check status
sudo systemctl status octoroute

# View logs
sudo journalctl -u octoroute -f

# Restart service
sudo systemctl restart octoroute

# Stop service
sudo systemctl stop octoroute
```

---

## Docker Deployment

### Dockerfile

Create `Dockerfile`:

```dockerfile
# Build stage
FROM rust:1.90-slim as builder

WORKDIR /build

# Copy manifest files
COPY Cargo.toml Cargo.lock ./
COPY rust-toolchain.toml ./

# Copy source
COPY src ./src
COPY benches ./benches

# Build release binary
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim

# Install CA certificates for HTTPS
RUN apt-get update && \
    apt-get install -y ca-certificates && \
    rm -rf /var/lib/apt/lists/*

# Create app user
RUN useradd -r -s /bin/false octoroute

# Copy binary from builder
COPY --from=builder /build/target/release/octoroute /usr/local/bin/

# Create config directory
RUN mkdir -p /etc/octoroute && \
    chown octoroute:octoroute /etc/octoroute

USER octoroute

WORKDIR /etc/octoroute

# Expose port
EXPOSE 3000

# Health check
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
  CMD ["/usr/local/bin/octoroute", "health"] || exit 1

CMD ["/usr/local/bin/octoroute"]
```

### Build Image

```bash
# Build image
docker build -t octoroute:latest .

# Verify image
docker images | grep octoroute
```

### Run Container

```bash
# Create config volume
mkdir -p /opt/octoroute/config
cp config.toml /opt/octoroute/config/

# Run container
docker run -d \
  --name octoroute \
  --restart unless-stopped \
  -p 3000:3000 \
  -v /opt/octoroute/config:/etc/octoroute:ro \
  -e RUST_LOG=octoroute=info \
  octoroute:latest

# Check logs
docker logs -f octoroute

# Check health
curl http://localhost:3000/health
```

### Docker Compose

Create `docker-compose.yml`:

```yaml
version: '3.8'

services:
  octoroute:
    build: .
    container_name: octoroute
    restart: unless-stopped
    ports:
      - "3000:3000"
    volumes:
      - ./config.toml:/etc/octoroute/config.toml:ro
      - octoroute-logs:/var/log/octoroute
    environment:
      - RUST_LOG=octoroute=info
      - OCTOROUTE_CONFIG=/etc/octoroute/config.toml
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:3000/health"]
      interval: 30s
      timeout: 10s
      retries: 3
      start_period: 5s

volumes:
  octoroute-logs:
```

**Run with Docker Compose**:

```bash
# Start service
docker-compose up -d

# View logs
docker-compose logs -f

# Stop service
docker-compose down
```

---

## Reverse Proxy

### Nginx

**Install nginx**:

```bash
sudo apt-get install nginx
```

**Configuration** (`/etc/nginx/sites-available/octoroute`):

```nginx
# HTTP → HTTPS redirect
server {
    listen 80;
    server_name octoroute.homelab.local;
    return 301 https://$server_name$request_uri;
}

# HTTPS server
server {
    listen 443 ssl http2;
    server_name octoroute.homelab.local;

    # SSL configuration (use Let's Encrypt or self-signed cert)
    ssl_certificate /etc/ssl/certs/octoroute.crt;
    ssl_certificate_key /etc/ssl/private/octoroute.key;
    ssl_protocols TLSv1.2 TLSv1.3;
    ssl_ciphers HIGH:!aNULL:!MD5;

    # Proxy to Octoroute
    location / {
        proxy_pass http://localhost:3000;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;

        # Timeout configuration (match Octoroute timeouts)
        proxy_connect_timeout 60s;
        proxy_send_timeout 120s;
        proxy_read_timeout 120s;
    }

    # Metrics endpoint with authentication
    location /metrics {
        auth_basic "Metrics";
        auth_basic_user_file /etc/nginx/.htpasswd;
        proxy_pass http://localhost:3000/metrics;
    }

    # Health check (unauthenticated)
    location /health {
        proxy_pass http://localhost:3000/health;
        access_log off;
    }
}
```

**Create htpasswd file**:

```bash
# Install apache2-utils
sudo apt-get install apache2-utils

# Create password file
sudo htpasswd -c /etc/nginx/.htpasswd prometheus

# Add more users
sudo htpasswd /etc/nginx/.htpasswd grafana
```

**Enable site**:

```bash
# Enable configuration
sudo ln -s /etc/nginx/sites-available/octoroute /etc/nginx/sites-enabled/

# Test configuration
sudo nginx -t

# Reload nginx
sudo systemctl reload nginx
```

---

### Caddy

**Install Caddy**:

```bash
sudo apt-get install -y debian-keyring debian-archive-keyring apt-transport-https
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' | sudo gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' | sudo tee /etc/apt/sources.list.d/caddy-stable.list
sudo apt-get update
sudo apt-get install caddy
```

**Configuration** (`/etc/caddy/Caddyfile`):

```caddy
octoroute.homelab.local {
    # Automatic HTTPS
    tls internal

    # Proxy to Octoroute
    reverse_proxy localhost:3000

    # Metrics with basic auth
    route /metrics {
        basicauth {
            prometheus $2a$14$...  # Generate with caddy hash-password
        }
        reverse_proxy localhost:3000
    }

    # Health check (no auth)
    route /health {
        reverse_proxy localhost:3000
    }
}
```

**Reload Caddy**:

```bash
sudo systemctl reload caddy
```

---

## Security Hardening

### Firewall Configuration

**Using ufw**:

```bash
# Allow SSH (be careful!)
sudo ufw allow 22/tcp

# Allow HTTPS only (nginx/Caddy handles SSL)
sudo ufw allow 443/tcp

# Deny direct access to Octoroute port
sudo ufw deny 3000/tcp

# Allow from Prometheus server (if not using reverse proxy auth)
sudo ufw allow from 192.168.1.10 to any port 3000 proto tcp

# Enable firewall
sudo ufw enable
```

**Using firewalld**:

```bash
# Allow HTTPS
sudo firewall-cmd --permanent --add-service=https

# Allow Octoroute from specific IP
sudo firewall-cmd --permanent --add-rich-rule='rule family="ipv4" source address="192.168.1.10" port protocol="tcp" port="3000" accept'

# Reload firewall
sudo firewall-cmd --reload
```

### Network Segmentation

Bind Octoroute to management network interface:

```toml
[server]
host = "192.168.100.10"  # Management network only
port = 3000
```

### Read-Only Configuration

```bash
# Make config read-only
sudo chmod 440 /etc/octoroute/config.toml
sudo chown octoroute:octoroute /etc/octoroute/config.toml
```

### Systemd Security Features

Already included in systemd service file:

- `NoNewPrivileges=true`: Prevents privilege escalation
- `PrivateTmp=true`: Isolated /tmp directory
- `ProtectSystem=strict`: Read-only system directories
- `ProtectHome=true`: No access to /home
- `ReadWritePaths=/var/log/octoroute`: Minimal write access

### AppArmor/SELinux

For additional mandatory access control, configure AppArmor or SELinux profiles:

**AppArmor** (Ubuntu/Debian):

```bash
# Create profile
sudo nano /etc/apparmor.d/usr.local.bin.octoroute

# Load profile
sudo apparmor_parser -r /etc/apparmor.d/usr.local.bin.octoroute
```

---

## Monitoring Setup

### Prometheus Configuration

Add Octoroute to `prometheus.yml`:

```yaml
scrape_configs:
  - job_name: 'octoroute'
    static_configs:
      - targets: ['octoroute.homelab.local:3000']
    metrics_path: '/metrics'
    scrape_interval: 15s
    scrape_timeout: 10s

    # If using nginx basic auth
    basic_auth:
      username: 'prometheus'
      password: 'your_password'
```

### Grafana Dashboard

Import dashboard or create custom panels:

1. Open Grafana
2. Create new dashboard
3. Add Prometheus data source
4. Add panels using PromQL queries from [observability.md](observability.md)

### Alerting

**Prometheus Alert Rules** (`/etc/prometheus/rules/octoroute.yml`):

```yaml
groups:
  - name: octoroute
    interval: 60s
    rules:
      # Alert on high routing latency
      - alert: OctorouteHighRoutingLatency
        expr: histogram_quantile(0.95, rate(octoroute_routing_duration_ms_bucket[5m])) > 1000
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "Octoroute routing latency is high"
          description: "95th percentile routing latency is {{ $value }}ms"

      # Alert on service down
      - alert: OctorouteDown
        expr: up{job="octoroute"} == 0
        for: 2m
        labels:
          severity: critical
        annotations:
          summary: "Octoroute is down"
          description: "Octoroute has been down for more than 2 minutes"

      # Alert on low rule router hit rate
      - alert: OctorouteLowRuleHitRate
        expr: |
          sum(rate(octoroute_requests_total{strategy="rule"}[5m]))
          /
          sum(rate(octoroute_requests_total[5m])) * 100 < 50
        for: 10m
        labels:
          severity: warning
        annotations:
          summary: "Octoroute rule router hit rate is low"
          description: "Only {{ $value }}% of requests hitting rule-based router"
```

---

## Backup and Recovery

### Configuration Backup

```bash
# Backup config
sudo cp /etc/octoroute/config.toml /etc/octoroute/config.toml.backup.$(date +%Y%m%d)

# Or automated daily backup
echo "0 2 * * * root cp /etc/octoroute/config.toml /etc/octoroute/config.toml.backup.\$(date +\%Y\%m\%d)" | sudo tee /etc/cron.d/octoroute-backup
```

### Logs Backup

```bash
# Archive old logs
sudo tar -czf /backup/octoroute-logs-$(date +%Y%m%d).tar.gz /var/log/octoroute/

# Automated weekly backup
echo "0 3 * * 0 root tar -czf /backup/octoroute-logs-\$(date +\%Y\%m\%d).tar.gz /var/log/octoroute/" | sudo tee /etc/cron.d/octoroute-logs-backup
```

### Recovery Procedure

```bash
# Stop service
sudo systemctl stop octoroute

# Restore config
sudo cp /etc/octoroute/config.toml.backup.20251120 /etc/octoroute/config.toml

# Restore binary (if needed)
sudo cp /backup/octoroute /usr/local/bin/

# Restart service
sudo systemctl start octoroute

# Verify
curl http://localhost:3000/health
```

---

## Updating

### Binary Update

```bash
# Stop service
sudo systemctl stop octoroute

# Backup old binary
sudo cp /usr/local/bin/octoroute /usr/local/bin/octoroute.backup.$(date +%Y%m%d)

# Build/download new binary
cargo build --release
sudo cp target/release/octoroute /usr/local/bin/

# Start service
sudo systemctl start octoroute

# Verify
sudo systemctl status octoroute
curl http://localhost:3000/health
```

### Docker Update

```bash
# Pull new image or rebuild
docker-compose build

# Restart with new image
docker-compose up -d

# Check logs
docker-compose logs -f
```

---

## Troubleshooting

### Service Won't Start

```bash
# Check systemd logs
sudo journalctl -u octoroute -n 50

# Check configuration
octoroute --config /etc/octoroute/config.toml --validate

# Check permissions
ls -la /etc/octoroute/config.toml
ls -la /usr/local/bin/octoroute
```

### Port Already in Use

```bash
# Check what's using port 3000
sudo lsof -i :3000

# Or with ss
sudo ss -tulpn | grep 3000

# Change port in config.toml
[server]
port = 3001
```

### Can't Reach Model Endpoints

```bash
# Test connectivity
curl http://model-endpoint:port/v1/models

# Check DNS resolution
nslookup model-endpoint

# Check firewall
sudo ufw status
```

### High Memory Usage

```bash
# Check memory usage
docker stats  # For Docker
ps aux | grep octoroute  # For binary

# Set memory limit in systemd
MemoryLimit=512M

# Monitor with htop
htop
```

---

## Best Practices

1. **Use systemd service**: Automatic restart on failure, logs management
2. **Run behind reverse proxy**: SSL termination, authentication, rate limiting
3. **Secure metrics endpoint**: Use basic auth or restrict by IP
4. **Monitor health**: Set up Prometheus alerts for service down and high latency
5. **Backup configuration**: Automated daily config backups
6. **Log rotation**: Configure logrotate for /var/log/octoroute
7. **Network segmentation**: Bind to management network if possible
8. **Update regularly**: Keep Octoroute and dependencies up to date
9. **Test before deploying**: Validate config changes in development first
10. **Document customizations**: Keep notes on deployment-specific configuration
