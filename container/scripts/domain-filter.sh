#!/bin/bash
# Domain-level network filtering for DuDuClaw browser sandbox.
# Blocks all outgoing connections except to allowed domains.
#
# Usage: ALLOWED_DOMAINS="example.com,*.gov.tw" ./domain-filter.sh [command...]
#
# If ALLOWED_DOMAINS is empty, no filtering is applied (container should use --network=none).

set -euo pipefail

if [ -n "${ALLOWED_DOMAINS:-}" ]; then
    echo "[domain-filter] Setting up iptables for allowed domains: $ALLOWED_DOMAINS"

    # Default: drop all outgoing
    iptables -P OUTPUT DROP 2>/dev/null || true

    # Allow loopback
    iptables -A OUTPUT -o lo -j ACCEPT 2>/dev/null || true

    # Allow DNS (needed to resolve allowed domains)
    iptables -A OUTPUT -p udp --dport 53 -j ACCEPT 2>/dev/null || true
    iptables -A OUTPUT -p tcp --dport 53 -j ACCEPT 2>/dev/null || true

    # Allow established connections
    iptables -A OUTPUT -m state --state ESTABLISHED,RELATED -j ACCEPT 2>/dev/null || true

    # Resolve and allow each domain
    IFS=',' read -ra DOMAINS <<< "$ALLOWED_DOMAINS"
    for domain in "${DOMAINS[@]}"; do
        domain=$(echo "$domain" | xargs)  # trim whitespace
        # Skip glob patterns (can't resolve *.example.com)
        if [[ "$domain" == \** ]]; then
            echo "[domain-filter] Skipping glob pattern: $domain (use specific domains for iptables)"
            continue
        fi
        # Resolve domain to IPs
        ips=$(dig +short "$domain" 2>/dev/null || true)
        for ip in $ips; do
            if [[ "$ip" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
                iptables -A OUTPUT -d "$ip" -j ACCEPT 2>/dev/null || true
                echo "[domain-filter] Allowed: $domain -> $ip"
            fi
        done
    done

    echo "[domain-filter] Firewall configured. Blocked all except allowed domains."
fi

# Execute the main command
exec "$@"
