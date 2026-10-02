#!/bin/bash
# Egress filtering for DuDuClaw containers (computer use, browser sandbox).
# Sourced by the computer-use entrypoint; used as the ENTRYPOINT (with the
# command to run as arguments) by Dockerfile.browser-sandbox.
#
# Modes, chosen by the environment:
#
#   ALLOWED_IPS=1.2.3.4,5.6.7.8   (computer use, tool-driven sessions)
#       The gateway resolved and vetted every allowlisted host itself and
#       pinned each one into /etc/hosts with `--add-host`. Only TCP 443 to
#       these addresses is allowed. No DNS at all: port 53 is REJECTed so a
#       lookup of any other name fails at once, and everything else ends in a
#       REJECT so a blocked request fails fast instead of hanging. Every entry
#       must be a dotted-quad IPv4 address or the script refuses to run.
#
#   ALLOWED_DOMAINS="example.com,*.gov.tw"   (browser sandbox)
#       Names are resolved inside the container (dig) and each resulting IPv4
#       address is allowed. DNS is allowed only to the resolvers listed in
#       /etc/resolv.conf. Globs are not enforceable here and are denied.
#
#   neither / empty
#       All egress denied (loopback to 127.0.0.1 / ::1 only).
#
# Loopback: in the pinned and deny-all modes only 127.0.0.1 and ::1 are
# reachable; Docker's embedded DNS (127.0.0.11, user-defined networks) and
# every other loopback address are REJECTed. The domain mode keeps all of
# loopback open because its resolver may be 127.0.0.11.
#
# ALLOWED_IPS and ALLOWED_DOMAINS together are refused (ambiguous).
#
# FAIL-CLOSED (P0-4 / invariant I5): IPv4 and IPv6 OUTPUT default to DROP in
# every mode. The policy needs CAP_NET_ADMIN, which Docker does not grant by
# default. If a policy or rule cannot be installed and the container has a
# route that is not via loopback, egress would be unfiltered, so the script
# exits non-zero (when sourced, that stops the calling entrypoint). With
# `--network=none` the kernel still lists down tunnel stubs (gre0, sit0, ...)
# in /sys/class/net, so the test is "is there any route not via lo", read
# from /proc/net/{route,ipv6_route}: none means no egress is possible and a
# failed iptables call is harmless.

set -euo pipefail

has_v4_route() {
    awk 'NR > 1 && $1 != "lo" { found = 1 } END { exit !found }' /proc/net/route 2>/dev/null
}
has_v6_route() {
    awk '$10 != "lo" { found = 1 } END { exit !found }' /proc/net/ipv6_route 2>/dev/null
}

NET_V4=0
NET_V6=0
has_v4_route && NET_V4=1
has_v6_route && NET_V6=1

df_fatal() {
    echo "[domain-filter] FATAL: $* — refusing to run with unfiltered egress." >&2
    exit 1
}

# ipt4 / ipt6 <args>: run one iptables / ip6tables command. A failure is
# fatal while a route of that family exists, harmless otherwise.
ipt4() {
    if ! iptables "$@" 2>/dev/null; then
        [ "$NET_V4" = 1 ] && df_fatal "iptables $* failed (needs --cap-add=NET_ADMIN) and the container has an IPv4 route"
    fi
    return 0
}
ipt6() {
    if ! command -v ip6tables >/dev/null 2>&1 || ! ip6tables "$@" 2>/dev/null; then
        [ "$NET_V6" = 1 ] && df_fatal "ip6tables $* failed (missing, or needs --cap-add=NET_ADMIN) and the container has an IPv6 route"
    fi
    return 0
}

# Strict dotted-quad IPv4: four decimal octets 0-255, no leading zeros.
valid_ipv4() {
    local ip="$1" o
    [[ "$ip" =~ ^([0-9]{1,3})\.([0-9]{1,3})\.([0-9]{1,3})\.([0-9]{1,3})$ ]] || return 1
    for o in "${BASH_REMATCH[@]:1}"; do
        [[ "$o" =~ ^(0|[1-9][0-9]*)$ ]] || return 1
        [ "$o" -le 255 ] || return 1
    done
    return 0
}

if [ -n "${ALLOWED_IPS:-}" ] && [ -n "${ALLOWED_DOMAINS:-}" ]; then
    df_fatal "both ALLOWED_IPS and ALLOWED_DOMAINS are set"
fi

# ALLOWED_IPS is validated before anything is installed.
ALLOWED_IP_LIST=()
if [ -n "${ALLOWED_IPS:-}" ]; then
    IFS=',' read -ra _ips <<< "$ALLOWED_IPS"
    for ip in "${_ips[@]}"; do
        if ! valid_ipv4 "$ip"; then
            echo "[domain-filter] FATAL: ALLOWED_IPS entry is not a dotted-quad IPv4 address: '$ip'" >&2
            exit 1
        fi
        ALLOWED_IP_LIST+=("$ip")
    done
    [ "${#ALLOWED_IP_LIST[@]}" -gt 0 ] || df_fatal "ALLOWED_IPS has no entries"
fi

# Default deny, both families, before anything else.
if [ "$NET_V4" = 0 ] && [ "$NET_V6" = 0 ]; then
    # --network=none (or no route): nothing can leave; install what we can.
    iptables -P OUTPUT DROP 2>/dev/null || echo "[domain-filter] iptables unavailable, but only loopback exists (--network=none) — no egress possible." >&2
    command -v ip6tables >/dev/null 2>&1 && { ip6tables -P OUTPUT DROP 2>/dev/null || true; }
    iptables -A OUTPUT -o lo -j ACCEPT 2>/dev/null || true
    command -v ip6tables >/dev/null 2>&1 && { ip6tables -A OUTPUT -o lo -j ACCEPT 2>/dev/null || true; }
else
    ipt4 -P OUTPUT DROP
    ipt6 -P OUTPUT DROP
    if [ -n "${ALLOWED_DOMAINS:-}" ]; then
        # Domain mode resolves names in the container, so the resolvers in
        # /etc/resolv.conf must stay reachable, including Docker's embedded
        # DNS on 127.0.0.11 (user-defined networks), which is on loopback.
        ipt4 -A OUTPUT -o lo -j ACCEPT
        ipt6 -A OUTPUT -o lo -j ACCEPT
    else
        # Pinned and deny-all modes: loopback to 127.0.0.1 / ::1 only (the
        # browser's DevTools port). On a user-defined Docker network
        # /etc/resolv.conf says 127.0.0.11, Docker's embedded DNS: its nat
        # OUTPUT rule DNATs 127.0.0.11:53 to 127.0.0.11:<random port>, which
        # changes the port and keeps the address, and the filter OUTPUT hook
        # runs after nat OUTPUT, so `-d 127.0.0.11` still matches here. A
        # blanket `-o lo -j ACCEPT` would let the browser resolve any name and
        # leak data through DNS queries; refused explicitly, and every other
        # loopback destination is refused too.
        #
        # The refusals themselves travel over loopback: a REJECT in this
        # chain answers with a TCP RST or an ICMP destination-unreachable
        # addressed to the container's own (non-127.0.0.1) address, which
        # the kernel routes via `lo` and sends through OUTPUT again. Without
        # the two ACCEPTs below the final `-o lo` REJECT drops them and a
        # refused connection hangs until the client gives up. They come
        # after the 127.0.0.11 rule, so they never reach the embedded DNS;
        # a RST or an unreachable error cannot open a connection or carry a
        # query to any listener; and only a process with CAP_NET_RAW (not
        # the browser's unprivileged user) could forge one.
        ipt4 -A OUTPUT -d 127.0.0.11 -j REJECT --reject-with icmp-port-unreachable
        ipt4 -A OUTPUT -o lo -d 127.0.0.1 -j ACCEPT
        ipt4 -A OUTPUT -o lo -p tcp --tcp-flags RST RST -j ACCEPT
        ipt4 -A OUTPUT -o lo -p icmp --icmp-type destination-unreachable -j ACCEPT
        ipt4 -A OUTPUT -o lo -j REJECT --reject-with icmp-port-unreachable
        ipt6 -A OUTPUT -o lo -d ::1 -j ACCEPT
        ipt6 -A OUTPUT -o lo -p tcp --tcp-flags RST RST -j ACCEPT
        ipt6 -A OUTPUT -o lo -p ipv6-icmp --icmpv6-type destination-unreachable -j ACCEPT
        ipt6 -A OUTPUT -o lo -j REJECT --reject-with icmp6-port-unreachable
    fi
fi

if [ "${#ALLOWED_IP_LIST[@]}" -gt 0 ]; then
    echo "[domain-filter] Pinned-address mode: TCP 443 to ${#ALLOWED_IP_LIST[@]} address(es), no DNS."
    ipt4 -A OUTPUT -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
    ipt6 -A OUTPUT -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
    for ip in "${ALLOWED_IP_LIST[@]}"; do
        ipt4 -A OUTPUT -d "$ip" -p tcp --dport 443 -j ACCEPT
    done
    # No DNS: fail lookups at once.
    ipt4 -A OUTPUT -p udp --dport 53 -j REJECT --reject-with icmp-port-unreachable
    ipt4 -A OUTPUT -p tcp --dport 53 -j REJECT --reject-with tcp-reset
    # Everything else fails fast.
    ipt4 -A OUTPUT -p tcp -j REJECT --reject-with tcp-reset
    ipt4 -A OUTPUT -j REJECT --reject-with icmp-port-unreachable
    ipt6 -A OUTPUT -p tcp -j REJECT --reject-with tcp-reset
    ipt6 -A OUTPUT -j REJECT --reject-with icmp6-port-unreachable
    echo "[domain-filter] Firewall configured."
    # Exec the wrapped command (ENTRYPOINT use) or return to the sourcing
    # entrypoint. Must stay at the top level: `return` in a function would
    # only leave the function.
    [ "$#" -gt 0 ] && exec "$@"
    return 0 2>/dev/null || exit 0
fi

if [ -z "${ALLOWED_DOMAINS:-}" ]; then
    echo "[domain-filter] ALLOWED_DOMAINS empty/unset — DENYING ALL egress (fail-closed)." >&2
    # Exec the wrapped command (ENTRYPOINT use) or return to the sourcing
    # entrypoint. Must stay at the top level: `return` in a function would
    # only leave the function.
    [ "$#" -gt 0 ] && exec "$@"
    return 0 2>/dev/null || exit 0
fi

echo "[domain-filter] Setting up iptables for allowed domains: $ALLOWED_DOMAINS"

ipt4 -A OUTPUT -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
ipt6 -A OUTPUT -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT

# DNS only to the resolvers in /etc/resolv.conf (needed to resolve the
# allowed domains), never to an arbitrary destination.
while read -r key value _; do
    [ "$key" = "nameserver" ] || continue
    if valid_ipv4 "$value"; then
        ipt4 -A OUTPUT -d "$value" -p udp --dport 53 -j ACCEPT
        ipt4 -A OUTPUT -d "$value" -p tcp --dport 53 -j ACCEPT
    elif [[ "$value" =~ ^[0-9a-fA-F:]+$ ]]; then
        ipt6 -A OUTPUT -d "$value" -p udp --dport 53 -j ACCEPT
        ipt6 -A OUTPUT -d "$value" -p tcp --dport 53 -j ACCEPT
    else
        echo "[domain-filter] ignoring unusable resolver entry in /etc/resolv.conf" >&2
    fi
done < /etc/resolv.conf

# Regex mirroring duduclaw_core::is_valid_egress_host (I10): a bare hostname or
# a single leading-wildcard glob; ASCII alnum + hyphen labels only. Anything
# with control bytes, `%`, `:`, `/`, `@`, or an IP-literal shape is rejected.
HOST_RE='^(\*\.)?([a-zA-Z0-9]([a-zA-Z0-9-]*[a-zA-Z0-9])?)(\.[a-zA-Z0-9]([a-zA-Z0-9-]*[a-zA-Z0-9])?)*$'
IPV4_RE='^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$'

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

    ips=$(dig +short "$domain" 2>/dev/null || true)
    for ip in $ips; do
        if valid_ipv4 "$ip"; then
            iptables -A OUTPUT -d "$ip" -j ACCEPT 2>/dev/null || true
            echo "[domain-filter] Allowed: $domain -> $ip"
        fi
    done
done

echo "[domain-filter] Firewall configured. Blocked all except allowed domains."

# Execute the main command (ENTRYPOINT use); return when sourced without one.
[ "$#" -gt 0 ] && exec "$@"
return 0 2>/dev/null || exit 0
