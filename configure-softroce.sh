#!/bin/bash
#
# Prepare an RDMA device for the integration tests on a CI runner.
#
# Preference order:
#   1. Soft-RoCE (rdma_rxe): a software RoCEv2 device on top of an ordinary NIC. This is the ideal
#      choice because it is deterministic and matches this crate's GID-based `handshake()`.
#   2. Whatever RDMA device the runner already exposes (the GitHub Actions runners back onto Azure
#      accelerated networking, which presents a real mlx5 device).
#
# The chosen device name is exported as IBVERBS_TEST_DEVICE (via $GITHUB_ENV) so the loopback tests
# target it. Full diagnostics are printed because the available RDMA device on hosted runners is a
# moving target.
#
# Written for the Ubuntu GitHub Actions runners; uses sudo. Note: no `set -e`, so that a missing
# rxe module does not abort before the fallback and diagnostics.

set -uxo pipefail

echo "Install RDMA userspace libraries, providers, and tools"
sudo apt-get update
sudo apt-get install -y rdma-core ibverbs-providers ibverbs-utils iproute2

echo "Try to provide and load the Soft-RoCE (rxe) kernel module"
sudo apt-get install -y "linux-modules-extra-$(uname -r)" || true
sudo depmod -a || true
if sudo modprobe rdma_rxe 2>/dev/null; then
    DEV=$(ip -o -4 route show to default | awk '{print $5; exit}')
    DEV=${DEV:-eth0}
    sudo rdma link add rxe0 type rxe netdev "$DEV" || true
else
    echo "rdma_rxe is not available for kernel $(uname -r); falling back to an existing device"
fi

echo "Diagnostics"
uname -r
find "/lib/modules/$(uname -r)" \( -name '*rxe*' -o -name '*siw*' \) -print || true
rdma link show || true
ibv_devices || true
ibv_devinfo || true

echo "Choose a device for the integration tests"
if [ -e /sys/class/infiniband/rxe0 ]; then
    DEVICE=rxe0
else
    DEVICE=$(ls /sys/class/infiniband 2>/dev/null | head -n1)
fi
echo "Selected RDMA device: ${DEVICE:-<none>}"
if [ -n "${DEVICE:-}" ] && [ -n "${GITHUB_ENV:-}" ]; then
    echo "IBVERBS_TEST_DEVICE=$DEVICE" >>"$GITHUB_ENV"
fi
