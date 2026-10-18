#!/usr/bin/env bash
set -euo pipefail

mkdir -p "$MC_TEST_DIR/diagnostics"
runtime_dir=$(mktemp -d)
trap 'rm -rf -- "$runtime_dir"' EXIT
export XDG_RUNTIME_DIR="$runtime_dir"
export ALSA_CONFIG_PATH="$runtime_dir/asound.conf"
cat > "$ALSA_CONFIG_PATH" <<'EOF'
pcm.!default {
    type null
}
EOF

lavapipe_drivers=(/usr/share/vulkan/icd.d/lvp_icd*.json)
if [[ ! -f "${lavapipe_drivers[0]}" ]]; then
    echo "Mesa Lavapipe Vulkan driver is missing" >&2
    exit 1
fi
export VK_DRIVER_FILES="${lavapipe_drivers[0]}"
export VK_ICD_FILENAMES="$VK_DRIVER_FILES"

{
    xrandr --query
    glxinfo -B
    vulkaninfo --summary
} 2>&1 | tee "$MC_TEST_DIR/diagnostics/display.log"

cargo test --locked --test installation install_and_launch -- --ignored --exact --nocapture
