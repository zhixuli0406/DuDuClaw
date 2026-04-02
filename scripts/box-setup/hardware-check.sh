#!/usr/bin/env bash
# DuDuClaw Box - Hardware Diagnostic
# Checks system compatibility and recommends configuration
set -euo pipefail

# ---------------------------------------------------------------------------
# Color helpers
# ---------------------------------------------------------------------------
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

ok()   { echo -e "${GREEN}✓${NC} $*"; }
warn() { echo -e "${YELLOW}!${NC} $*"; }
err()  { echo -e "${RED}✗${NC} $*"; }

# ---------------------------------------------------------------------------
# Gather system information
# ---------------------------------------------------------------------------
echo -e "${BOLD}DuDuClaw Box — Hardware Diagnostic${NC}"
echo -e "${BOLD}$(printf '=%.0s' {1..50})${NC}"
echo ""

# macOS version
macos_version=$(sw_vers -productVersion)
macos_build=$(sw_vers -buildVersion)
major_version=$(echo "$macos_version" | cut -d. -f1)

# Chip / CPU
arch=$(uname -m)
chip="Unknown"
if [[ "$arch" == "arm64" ]]; then
  chip=$(sysctl -n machdep.cpu.brand_string 2>/dev/null || echo "Apple Silicon (unknown)")
else
  chip=$(sysctl -n machdep.cpu.brand_string 2>/dev/null || echo "Intel (unknown)")
fi

# CPU cores
cpu_perf_cores=$(sysctl -n hw.perflevel0.logicalcpu 2>/dev/null || echo "?")
cpu_eff_cores=$(sysctl -n hw.perflevel1.logicalcpu 2>/dev/null || echo "?")
cpu_total=$(sysctl -n hw.logicalcpu 2>/dev/null || echo "?")

# RAM
ram_bytes=$(sysctl -n hw.memsize 2>/dev/null || echo "0")
ram_gb=$((ram_bytes / 1073741824))

# Available RAM (approximate)
available_pages=$(vm_stat 2>/dev/null | awk '/Pages free/ {gsub(/\./,"",$3); print $3}')
page_size=$(vm_stat 2>/dev/null | awk '/page size/ {print $8}')
if [[ -n "$available_pages" && -n "$page_size" ]]; then
  available_gb=$(( (available_pages * page_size) / 1073741824 ))
else
  available_gb="?"
fi

# GPU / Metal
gpu_info="N/A"
metal_support="No"
if system_profiler SPDisplaysDataType &>/dev/null; then
  gpu_info=$(system_profiler SPDisplaysDataType 2>/dev/null | grep -E "Chipset Model|Chip Model" | head -1 | sed 's/.*: //' || echo "N/A")
  metal_family=$(system_profiler SPDisplaysDataType 2>/dev/null | grep "Metal Family" | head -1 | sed 's/.*: //' || echo "")
  if [[ -n "$metal_family" ]]; then
    metal_support="Yes ($metal_family)"
  fi
fi

# Disk space
disk_total=$(df -h / | awk 'NR==2 {print $2}')
disk_available=$(df -h / | awk 'NR==2 {print $4}')
disk_used_pct=$(df -h / | awk 'NR==2 {print $5}')

# Docker / Podman
docker_status="Not installed"
if command -v docker &>/dev/null; then
  docker_ver=$(docker --version 2>/dev/null | head -1)
  if docker info &>/dev/null 2>&1; then
    docker_status="Running ($docker_ver)"
  else
    docker_status="Installed but not running ($docker_ver)"
  fi
fi

podman_status="Not installed"
if command -v podman &>/dev/null; then
  podman_ver=$(podman --version 2>/dev/null | head -1)
  podman_status="Installed ($podman_ver)"
fi

# Rust
rust_status="Not installed"
if command -v rustc &>/dev/null; then
  rust_status="$(rustc --version)"
fi

# Node
node_status="Not installed"
if command -v node &>/dev/null; then
  node_status="$(node --version)"
fi

# ---------------------------------------------------------------------------
# Recommendations
# ---------------------------------------------------------------------------
# Max model size: ~70% of total RAM
max_model_gb=$(( ram_gb * 70 / 100 ))

# Recommended backend
recommended_backend="CPU"
if [[ "$metal_support" != "No" ]]; then
  recommended_backend="Metal (GPU accelerated)"
fi

# Tier recommendation
tier="Mini (7B models)"
if [[ "$ram_gb" -ge 128 ]]; then
  tier="Cluster (70B+ models, Exo P2P capable)"
elif [[ "$ram_gb" -ge 64 ]]; then
  tier="Pro (32B-70B models)"
elif [[ "$ram_gb" -ge 32 ]]; then
  tier="Pro (14B-32B models)"
elif [[ "$ram_gb" -ge 16 ]]; then
  tier="Mini (7B-14B models)"
fi

# macOS compat
macos_compat="Supported"
if [[ "$major_version" -lt 14 ]]; then
  macos_compat="Unsupported (requires macOS 14+)"
fi

# ---------------------------------------------------------------------------
# Output formatted table
# ---------------------------------------------------------------------------
fmt="  %-28s %s\n"

echo -e "${BOLD}System Information${NC}"
echo -e "$(printf -- '-%.0s' {1..50})"
printf "$fmt" "macOS Version:" "$macos_version ($macos_build)"
printf "$fmt" "Compatibility:" "$macos_compat"
printf "$fmt" "Architecture:" "$arch"
printf "$fmt" "Chip:" "$chip"
printf "$fmt" "CPU Cores (P/E/Total):" "${cpu_perf_cores}P / ${cpu_eff_cores}E / ${cpu_total} total"
echo ""

echo -e "${BOLD}Memory${NC}"
echo -e "$(printf -- '-%.0s' {1..50})"
printf "$fmt" "Total RAM:" "${ram_gb} GB"
printf "$fmt" "Available RAM (approx):" "${available_gb} GB"
echo ""

echo -e "${BOLD}GPU${NC}"
echo -e "$(printf -- '-%.0s' {1..50})"
printf "$fmt" "GPU:" "$gpu_info"
printf "$fmt" "Metal Support:" "$metal_support"
echo ""

echo -e "${BOLD}Storage${NC}"
echo -e "$(printf -- '-%.0s' {1..50})"
printf "$fmt" "Disk Total:" "$disk_total"
printf "$fmt" "Disk Available:" "$disk_available"
printf "$fmt" "Disk Used:" "$disk_used_pct"
echo ""

echo -e "${BOLD}Software${NC}"
echo -e "$(printf -- '-%.0s' {1..50})"
printf "$fmt" "Docker:" "$docker_status"
printf "$fmt" "Podman:" "$podman_status"
printf "$fmt" "Rust:" "$rust_status"
printf "$fmt" "Node.js:" "$node_status"
echo ""

echo -e "${BOLD}DuDuClaw Recommendations${NC}"
echo -e "$(printf -- '-%.0s' {1..50})"
printf "$fmt" "Max Model Size:" "~${max_model_gb} GB (70% of RAM)"
printf "$fmt" "Recommended Backend:" "$recommended_backend"
printf "$fmt" "Suitable Tier:" "$tier"
echo ""

# ---------------------------------------------------------------------------
# Warnings
# ---------------------------------------------------------------------------
has_warning=false

if [[ "$major_version" -lt 14 ]]; then
  err "macOS $macos_version is not supported. Please upgrade to macOS 14 (Sonoma) or later."
  has_warning=true
fi

if [[ "$arch" != "arm64" ]]; then
  warn "Intel Mac detected. Local inference will be CPU-only and significantly slower."
  has_warning=true
fi

if [[ "$ram_gb" -lt 8 ]]; then
  err "Less than 8 GB RAM. Local inference may not work."
  has_warning=true
elif [[ "$ram_gb" -lt 16 ]]; then
  warn "16 GB+ RAM recommended for comfortable local inference."
  has_warning=true
fi

if [[ "$metal_support" == "No" ]]; then
  warn "No Metal support detected. GPU acceleration unavailable."
  has_warning=true
fi

disk_avail_gb=$(df -g / | awk 'NR==2 {print $4}')
if [[ "$disk_avail_gb" -lt 20 ]]; then
  warn "Less than 20 GB disk space. Models require significant storage."
  has_warning=true
fi

if [[ "$has_warning" == false ]]; then
  ok "All checks passed. System is ready for DuDuClaw Box."
fi
echo ""
