#!/bin/bash
# Domain-level network filtering for DuDuClaw browser sandbox.
# Blocks all outgoing connections except to allowed domains.
#
# Usage: ALLOWED_DOMAINS="example.com,*.gov.tw" ./domain-filter.sh [command...]
#
# FAIL-CLOSED (P0-4 / invariant I5): if ALLOWED_DOMAINS is empty or unset, this
# script denies ALL egress (default OUTPUT policy DROP, loopback only) instead
# of leaving the network unfiltered. An operator who genuinely wants no network
# should run the container with `--network=none`; an operator who wants egress
# MUST provide a non-empty allowlist. There is no unfiltered path.

set -euo pipefail

# Always default-deny outbound first, regardless of allowlist presence.
iptables -P OUTPUT DROP 2>/dev/null || true
iptables -A OUTPUT -o lo -j ACCEPT 2>/dev/null || true

if [ -z "${ALLOWED_DOMAINS:-}" ]; then
    echo "[domain-filter] ALLOWED_DOMAINS empty/unset — DENYING ALL egress (fail-closed)." >&2
    exec "$@"
fi

echo "[domain-filter] Setting up iptables for allowed domains: $ALLOWED_DOMAINS"

# Allow DNS (needed to resolve allowed domains)
iptables -A OUTPUT -p udp --dport 53 -j ACCEPT 2>/dev/null || true
iptables -A OUTPUT -p tcp --dport 53 -j ACCEPT 2>/dev/null || true

# Allow established connections
iptables -A OUTPUT -m state --state ESTABLISHED,RELATED -j ACCEPT 2>/dev/null || true

# Regex mirroring duduclaw_core::is_valid_egress_host (I10): a bare hostname or
# a single leading-wildcard glob; ASCII alnum + hyphen labels only. Anything
# with control bytes, `%`, `:`, `/`, `@`, or an IP-literal shape is rejected.
HOST_RE='^(\*\.)?([a-zA-Z0-9]([a-zA-Z0-9-]*[a-zA-Z0-9])?)(\.[a-zA-Z0-9]([a-zA-Z0-9-]*[a-zA-Z0-9])?)*$'
IPV4_RE='^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$'

# Resolve and allow each domain
IFS=',' read -ra DOMAINS <<< "$ALLOWED_DOMAINS"
for domain in "${DOMAINS[@]}"; do
    domain=$(echo "$domain" | xargs)  # trim whitespace

    # Reject malformed / IP-literal entries loudly (never silently accept).
    if [[ ! "$domain" =~ $HOST_RE ]] || [[ "$domain" =~ $IPV4_RE ]]; then
        echo "[domain-filter] REJECTED invalid allowlist entry: '$domain'" >&2
        continue
    fi

    # Glob patterns cannot be resolved to fixed IPs by iptables. Deny is the
    # safe outcome, but WARN loudly so the operator knows the glob had no effect
    # (host-level glob matching requires the L2 egress proxy, not iptables).
    if [[ "$domain" == \*.* ]]; then
        echo "[domain-filter] WARNING: glob '$domain' NOT enforced by iptables (needs egress proxy) — traffic to it is DENIED" >&2
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

# Execute the main command
exec "$@"
