# Guest virtio-net: real networking through the pure-Rust interpreter

## Transport choice: virtio-mmio, not virtio-pci

The emulated NIC speaks virtio-mmio (virtio spec v1.x, modern v2 register
file) at MMIO 0xFEBF1000, IRQ 11, one page above the block device window.
Reasons, checked against the pinned kernel config
(`guest/x86_64/out/downloads/config-6.18.35-0-virt`):

- The pinned virt kernel has the virtio-mmio **transport built in**
  (`CONFIG_VIRTIO_MMIO=y`) and supports command-line device discovery
  (`CONFIG_VIRTIO_MMIO_CMDLINE_DEVICES=y`). The provider appends
  `virtio_mmio.device=1K@0xfebf1000:11` to the kernel command line (the
  parameter may be repeated once per device; the block device is the first
  declaration), so the guest finds the NIC with no PCI work at all.
- virtio-pci would require PCI enumeration, BAR mapping, capability
  parsing, and MSI/MSI-X or INTx routing before the first queue could be
  configured. None of that exists in the interpreter.
- The split virtqueues live in ordinary guest RAM; the device walks them
  with checked arithmetic only.
- IRQ 11 is identity-mapped to I/O APIC pin 11 through the published MADT
  (PCAT_COMPAT), exactly like the block device's pin 10.

The guest's `virtio_net` driver is a module in the pinned kernel; the
container initramfs bakes it (plus its failover dependencies) in and
insmods it, see `guest/x86_64/build-container-initramfs.sh`.

## What is implemented

### The device (rish-softvm-core, `crates/rish-softvm-core/src/virtio/net.rs`)

- Modern virtio-mmio register file shared with the block device, plus the
  virtio-net config space: the fixed MAC `52:54:00:12:34:56` and a status
  register reporting `VIRTIO_NET_S_LINK_UP`.
- Offered feature bits: `VIRTIO_F_VERSION_1`, `VIRTIO_NET_F_MAC`,
  `VIRTIO_NET_F_STATUS`. Deliberately absent: checksum/GSO offloads,
  `VIRTIO_NET_F_MRG_RXBUF`, `VIRTIO_NET_F_MTU`, multi-queue, control
  virtqueue, VLAN filtering.
- Two split virtqueues (at most 128 descriptors each): queue 0 receives,
  queue 1 transmits. The per-frame header is the **12-byte**
  `virtio_net_hdr_mrg_rxbuf` the pinned kernel's virtio_net uses whenever
  `VIRTIO_F_VERSION_1` is negotiated (verified against the v6.18
  `drivers/net/virtio_net.c` source and the guest's live buffers); the
  driver may push it inline with the TX data, and both TX shapes are
  accepted.
- Fail-closed queue servicing, same discipline as virtio-blk: the whole
  descriptor chain is validated before anything is drained; out-of-RAM
  addresses, cyclic or overlong chains, and wrong descriptor directions
  latch a **sticky device fault** and stop the device. Frames larger than
  the 1514-byte frame limit (1500-byte MTU) are dropped and counted, never
  truncated; their buffers still complete so the rings cannot wedge.
- Receive buffers the driver posted are filled from a bounded backlog
  (128 frames, drop-oldest, counted) with a 12-byte zero header; the
  backend is polled on host wall clock (1 ms throttle) so the 64-
  instruction device tick does not turn into a syscall storm. Used-ring
  completions raise one interrupt edge on pin 11.

### The backend (rish-softvm-core, `crates/rish-softvm-core/src/net/`)

A slirp-style user-mode backend in safe Rust, standard library only (no
new dependencies). It terminates the guest's frames on host sockets:

- **ARP**: answers requests for the gateway address (10.0.2.2).
- **IPv4**: unfragmented packets with a verified header checksum only.
- **ICMP**: echo replies for the gateway address, so `ping 10.0.2.2`
  proves the link. Echo to any other address is dropped (there is no NAT
  for ICMP and no pretending that a remote host answered).
- **UDP/DNS**: queries to gateway:53 are forwarded to the host resolver
  (`/etc/resolv.conf`, first nameserver) with transaction ids rewritten so
  concurrent queries cannot collide; answers are relayed with recomputed
  checksums. Unanswered queries expire after 30 s. Truncated answers are
  relayed as-is (the guest's TCP retry for large responses is not
  terminated). Any other UDP is dropped and counted.
- **TCP**: outbound proxy. The guest's SYN opens a host socket on a
  dedicated thread; sequence numbers, acknowledgements, windows, MSS
  clamping (1460), and the FIN handshake are translated, and the remote
  host's address is preserved in the segments the guest sees, like slirp's
  NAT. Guest data crosses a bounded channel onto the host thread (a full
  channel holds the segment; the guest's own retransmission retries); host
  data crosses a bounded channel off the thread (a full channel stops the
  thread from reading), so the interpreter thread never blocks on a host
  socket.
- Static guest configuration: 10.0.2.15/24, gateway and DNS at 10.0.2.2,
  set by the overlay init. This was chosen over a DHCP server because it
  is deterministic, needs no DHCP state machine on the host side, and the
  guest already ships busybox `ip`.

### Provider wiring (`crates/rish-softvm-x86_64/src/pure_rust.rs`)

`PureRustProvider::create` attaches the slirp backend when
`network_mode == user-nat`, appends the second `virtio_mmio.device`
fragment, and declares `abi::FEATURE_USER_NETWORK`. Any other network mode
value fails closed. A command line that already declares `virtio_mmio.device`
is still rejected (the provider attaches both of its own windows). The
`rish_vm_run_docker_json` request surface gained a `network` field
(`"disabled"` or `"user-nat"`); the `vm_smoke` example selects it with
`RISH_NETWORK=user-nat`.

### Guest driver availability (`guest/x86_64/`)

`CONFIG_VIRTIO_NET=m` in the pinned kernel, and the driver depends on
`net_failover`, which depends on `failover` (both also modules).
`build-container-initramfs.sh` now bakes `virtio_net.ko`, `net_failover.ko`,
and `failover.ko` from the pinned netboot initramfs (the same asset and
deterministic gzip+cpio mechanism the block modules use), plus the Alpine
CA bundle (`etc/ssl/certs/ca-certificates.crt`) so the guest's apk can
verify HTTPS mirrors. The overlay init insmods the failover stack in
dependency order, then brings `eth0` up with the static addressing above;
every step tolerates a missing device so a disk-less, NIC-less boot still
reaches the agent.

## Verified end to end (real runs, not inference)

One boot proves all five layers in sequence (link/address, gateway ping,
DNS, TCP to a real host, and `apk update` against the real Alpine mirror).
Boot command: `cargo run --release -p rish-ffi --example vm_smoke` with
`RISH_NETWORK=user-nat`, `RISH_BOOT_BUDGET=80000000000`,
`RISH_HANDSHAKE_BUDGET=200000000000`, and the rebuilt initramfs
(sha256 `f45151656e71b3a3f4285023923a11bf37eec09fb4e2d76fbd74ed9952843492`,
deterministic across two builds). Response: `ok`: true, `exit_code`: 0,
`boot_units`: 1,352,000,000. Raw `stdout`:

```text
==ip_link==
1: lo: <LOOPBACK> mtu 65536 qdisc noop state DOWN qlen 1000
    link/loopback 00:00:00:00:00:00 brd 00:00:00:00:00:00
2: eth0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 qdisc pfifo_fast state UP qlen 1000
    link/ether 52:54:00:12:34:56 brd ff:ff:ff:ff:ff:ff
==ip_addr==
1: lo: <LOOPBACK> mtu 65536 qdisc noop state DOWN qlen 1000
    link/loopback 00:00:00:00:00:00 brd 00:00:00:00:00:00
2: eth0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 qdisc pfifo_fast state UP qlen 1000
    link/ether 52:54:00:12:34:56 brd ff:ff:ff:ff:ff:ff
    inet 10.0.2.15/24 scope global eth0
       valid_lft forever preferred_lft forever
    inet6 fe80::5054:ff:fe12:3456/64 scope link tentative
       valid_lft forever preferred_lft forever
==ping_gw==
PING 10.0.2.2 (10.0.2.2): 56 data bytes
64 bytes from 10.0.2.2: seq=0 ttl=64 time=7.333 ms
64 bytes from 10.0.2.2: seq=1 ttl=64 time=4.291 ms
64 bytes from 10.0.2.2: seq=2 ttl=64 time=5.026 ms

--- 10.0.2.2 ping statistics ---
3 packets transmitted, 3 packets received, 0% packet loss
round-trip min/avg/max = 4.291/5.550/7.333 ms
==nslookup==
Server:		10.0.2.2
Address:	10.0.2.2:53

Non-authoritative answer:
dl-cdn.alpinelinux.org	canonical name = dualstack.j.sni.global.fastly.net
Name:	dualstack.j.sni.global.fastly.net
Address: 146.75.114.132

Non-authoritative answer:
dl-cdn.alpinelinux.org	canonical name = dualstack.j.sni.global.fastly.net
Name:	dualstack.j.sni.global.fastly.net
Address: 2a04:4e42:8c::644
==wget_tcp==
WGET_EXIT=0
559 /tmp/page
<!doctype html><html lang="en"><head><title>Example Domain</title><link rel="icon" href="data:,"><me
==apk_update==
https://dl-cdn.alpinelinux.org/alpine/v3.24/main
v3.24.1-452-ge2a3b3432b1 [https://dl-cdn.alpinelinux.org/alpine/v3.24/main]
OK: 5962 distinct packages available
APK_UPDATE_EXIT=0
==irqs==
 10:          1  IO-APIC  10-edge      virtio0
 11:        168  IO-APIC  11-edge      virtio1
==DONE==
```

The address, MAC, MTU, and state in (a) match the configured device
exactly. (b) is a real round trip through ARP + ICMP on the emulated wire.
(c) is a real DNS round trip: the query crossed the guest's resolver, the
emulated NIC, and the backend's forwarder to the host resolver, and the
CNAME/A/AAAA answers came back. (d) downloaded 559 bytes of the real
example.com index page over the backend's TCP proxy. (e) fetched, verified
(HTTPS, real CA chain), and parsed the real Alpine v3.24 main index --
5,962 packages -- with `apk`'s own fetcher.

### Bulk integrity and repeatability

- The guest downloaded the same `APKINDEX.tar.gz` (528,320 bytes) over
  plain HTTP through the same TCP path; its in-guest `md5sum` matched the
  host-side download byte for byte
  (`49b42f25cc3954542206bd437b624710`).
- `apk update` was run 14 more times across two boots; 13 succeeded and
  one failed mid-transfer with an `I/O error` from the guest's TLS stack.
  The failure is intermittent, tied to real network conditions: under
  burst loss the backend's single retransmission timer used to stall long
  enough for the remote side to give up. It now resends every
  unacknowledged segment per timeout (bounded, idempotent), which cut the
  recovery time; the remaining residual is the honest limit of a TCP
  bridge without fast retransmit or SACK (see the inventory below).

### Offline regression (unchanged capability)

The offline repository still works from the same rebuilt initramfs, with
`network` disabled:

```text
==apk_add_tree==
(1/2) Installing musl (1.2.6-r2)
(2/2) Installing tree (2.3.2-r0)
OK: 743736 B in 2 packages
APK_ADD_EXIT=0
/opt/rish-apk-repo/main
└── x86_64
    ├── APKINDEX.tar.gz
    ├── musl-1.2.6-r2.apk
    └── tree-2.3.2-r0.apk

2 directories, 3 files
==DONE==
```

## Interpreter defects this work uncovered (and fixed)

Following the virtio-blk precedent of reporting only what the live guest
showed, five real defects surfaced while bringing the NIC up; each was
fixed with a regression test:

1. **Byte-granular MMIO reads returned the unshifted register word.** The
   virtio-mmio driver reads the MAC and status bytes one at a time; the
   guest saw MAC `52:52:52:52:34:34` and carrier DOWN. Reads now shift the
   word by the byte offset (`virtio/net.rs`, test
   `byte_offsets_shift_the_register_word`).
2. **QueueNotify was decoded with the blk single-queue semantics.** The
   written value IS the queue index, not a comparison against the queue
   selector; TX kicks were silently dropped. (`virtio/net.rs`,
   `submit_tx` test now notifies with the selector left on the RX queue.)
3. **The virtio-net header is 12 bytes with VIRTIO_F_VERSION_1**, not 10:
   kernel 6.18's virtnet_probe uses `sizeof(struct
   virtio_net_hdr_mrg_rxbuf)` whenever the modern transport is negotiated,
   and pushes it inline with the TX data (verified against the v6.18
   `drivers/net/virtio_net.c` source and the guest's live buffers).
4. **The backend's IP replies were not framed as Ethernet**, and the ARP
   reply claimed the guest's own MAC as the gateway's: the guest learned
   its own address for 10.0.2.2 and unicast to itself. Replies are now
   wrapped (gateway MAC `52:55:0a:00:02:02` to guest MAC) and the filter
   accepts both MACs plus broadcast.
5. **The guest RTC had no date fields** (seconds/minutes/hours only), so
   the kernel fell back to 1999-11-30 and every TLS certificate was
   "not yet valid"; the provider also booted with epoch 0. The CMOS now
   serves day-of-week/day/month/year/century from the epoch and the
   provider boots with the host wall clock. A related TCP defect kept the
   SYN-ACK permanently unacknowledged (`our_una` started one sequence
   number too high), retransmitting the handshake until the connection
   reset; fixed with a regression assertion in the loopback test.

## Explicitly not implemented / not verified (fail-closed inventory)

- **TCP is a minimal, honest state machine, not a complete one**: no
  out-of-order reassembly (ahead-of-sequence segments are ignored and the
  sender retransmits), no delayed ACK, no fast retransmit, no SACK, no
  window scaling negotiation (the backend never sends the WS option, which
  disables scaling per RFC 7323), no zero-window probing, no PAWS, no
  congestion control (the host kernel's stack does that on the real
  network), and a single 750 ms / 8-try retransmission timer per
  connection that resends the oldest unacknowledged segment. It is a
  store-and-forward bridge, verified against a real remote mirror below —
  it is not a general-purpose TCP endpoint for adversarial networks.
- **No inbound (host-to-guest) TCP**: there is no listener; only
  connections the guest initiates exist. Port forwarding is not
  implemented.
- **No NAT for arbitrary UDP or ICMP**: only DNS (gateway:53) and gateway
  echo are terminated; other UDP/ICMP is dropped. No ICMP errors are
  generated, and no IP fragmentation/reassembly exists (MSS is clamped so
  TCP never fragments; DNS and ICMP payloads are far below the MTU).
- **DNS is forward-only over UDP**: answers that do not fit 512 bytes may
  be truncated by the resolver, and the guest's TCP retry is not
  terminated. No DNS cache, no DNSSEC handling, no mDNS.
- **IPv6 is not implemented** at any layer: IPv6 frames are dropped and
  counted; the guest gets IPv4 only.
- No DHCP server (static guest configuration, see above), no
  suspend/checkpoint of network state, no MTU reporting feature bit, no
  checksum offloads, no TSO/GSO, no multi-queue or RSS, no VLAN, no
  virtio-net control virtqueue, no live migration of backend state.
- The host threads use blocking `TcpStream::connect`: an unreachable
  remote can stall one connection for the host kernel's connect timeout.
  Backend retransmission timers run on **host** wall clock while guest
  time is interpreted; that mismatch is deliberate (host sockets live in
  host time) but means guest-visible timing is not a faithful emulation.
- The TCTI/QEMU provider path is unchanged and its network mode remains
  unverified here.

## Roadmap meaning (P3/P4/P5/P6)

This closes the longest-open P3 item: the pure-Rust interpreter now has a
verified user-mode network data plane (the roadmap line "用户态 NAT、DNS、
TCP/UDP 端口转发" — the port-forwarding part of that line is explicitly
not done, see above). For P4 it makes the guest able to pull and reach
services, the precondition for any OCI-runtime work that downloads images
inside the guest. P5's veth/bridge/TUN/NAT item is unchanged — that item
is about guest-internal networking and privileged policies, not this
transport. P6's DinD networking (containers reaching the network from
inside docker-in-the-guest) now has a working default route to build on,
though it will inherit every TCP limitation listed above until the state
machine grows reassembly, window scaling, and port forwarding.

## Files

- `crates/rish-softvm-core/src/virtio/net.rs` — the device (register file,
  RX/TX queues, fail-closed servicing).
- `crates/rish-softvm-core/src/net/` — the backend: `mod.rs` (dispatch,
  counters), `ether.rs` (Ethernet/ARP), `ipv4.rs` (IPv4/ICMP/checksums),
  `udp.rs` (DNS forwarder), `tcp.rs` + `tcp_segment.rs` (TCP state machine
  and wire format), `host.rs` (per-connection host socket thread).
- `crates/rish-softvm-x86_64/src/pure_rust.rs` — provider attachment and
  the two virtio_mmio command-line fragments.
- `guest/x86_64/build-container-initramfs.sh`,
  `guest/x86_64/container-overlay/init` — module baking and eth0 bring-up.
