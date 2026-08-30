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
- `apk update` was then run repeatedly to measure reliability: 18 more
  attempts across four boots, 16 successes and 2 failures (one mid-transfer
  `I/O error`, one handshake `TLS: unspecified error` from the guest's TLS
  stack, each on a different boot). An immediate retry succeeded in every
  observed case (3/3). The attribution above ("real packet loss plus a
  single retransmission timer") was an unverified inference; the
  independent review found a concrete alternative explanation and it is
  fixed in the hardening round below: a legitimate host-side burst could
  push the per-connection pending buffer past its cap within one poll, and
  the backend then reset the connection mid-transfer. The cap is now a
  backpressure point, never a kill switch, so that failure mode is gone.
  Four post-fix attempts (one plus three more on a second boot) all
  succeeded; that is consistent with the fix but too small a sample to
  claim the 2/18 are fully explained -- the handshake failure in particular
  has no matching defect here and may be real network noise.

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

## Hardening round (independent review, virtio-net)

The block-device hardening round ended with an independent review of the
network device, which confirmed four P1 defects and a P2 cluster across two
untrusted input surfaces: the guest drives the descriptor rings from its
own memory, and the backend parses bytes that arrived from the real
network. Everything below is fixed with regression tests written red first
(panic/hang/assert evidence against the pre-fix code), and the workspace
suite went from 589 to 615 passing tests.

### Shared virtqueue validation skeleton

"Unbounded avail-ring delta" and "data movement before the whole chain
validated" had each been patched once per device. `virtio/queue.rs` now
owns the shared code both devices use:

- `avail_delta`: the driver can never publish more entries than the ring
  holds between two drains; a larger delta fails the device closed instead
  of replaying one head per phantom slot.
- `drain_available`: validates the three rings, bounds the delta, walks
  each head through the device's processor, and publishes one used entry
  per completion. The block drain and the net transmit drain both ride it.
- `validated_chain`: the phase-1 gate. A chain is walked (ring, length,
  and cycle bounds) and every descriptor buffer is proven inside guest RAM
  before the device moves a single byte; `require_all_device_readable` /
  `require_all_device_writable` and `total_bytes` cover the direction and
  length checks. Only chains that passed phase 1 are ever touched.

Not shared (deliberately): the receive drain keeps its own per-frame loop
because each head consumes one backlogged frame and the ring must never be
completed without data; the request-type semantics (blk header/sector
layout, net 12-byte virtio_net_hdr/frame layout) stay in each device.

### P1 fixes

- **P1-A short DNS answers (panic).** The transaction id was read from the
  4096-byte receive buffer without checking the datagram length, and the
  guest-id rewrite indexed `answer[0..2]` on an empty vec: a zero- or
  1-byte datagram from the configured resolver panicked the host. Answers
  shorter than the 12-byte DNS header are now dropped and counted, and the
  id is read from the bytes that actually arrived. Test:
  `a_short_resolver_datagram_is_dropped_instead_of_panicking` (red: exact
  panic at the old indexing site).
- **P1-B DNS id exhaustion (permanent hang).** With all 65536 ids held,
  `allocate_id` spun forever -- and the expiry sweep runs on the same
  interpreter thread. Expired queries are now swept before allocation, the
  pending table is capped at 1024, and the allocator returns None (query
  dropped and counted) instead of spinning. Tests:
  `id_exhaustion_fails_closed_instead_of_hanging`,
  `a_query_beyond_the_pending_cap_is_dropped_not_forwarded`,
  `expired_queries_are_swept_before_allocating_an_id` (red: the first two
  hang on the pre-fix code; the cap test fails fast).
- **P1-C RX zero-length descriptor (underflow) and write-before-validation.**
  A zero-length writable descriptor at guest address 0 made the device write
  an empty slice whose page-counter bump computed `start + len - 1` with
  len == 0: debug panic, release ~4.5e15-page hang. `bump_page_counters`
  now returns early on len == 0, the receive path validates the whole chain
  (in-bounds, device-writable) before writing, and the header+frame are
  laid out contiguously from chain offset 0, which also fixes the split
  shape (the frame used to start at descriptor 1 offset 0, on top of the
  header tail). Tests: `zero_length_device_writes_do_not_underflow_the_page_counters`
  (red: overflow panic), `a_zero_length_receive_descriptor_keeps_header_and_frame_layout`,
  `a_split_receive_chain_places_header_and_frame_contiguously`,
  `a_receive_chain_fails_before_writing_when_a_later_descriptor_is_out_of_ram`.
- **P1-D unbounded, uncancellable host threads.** Every SYN spawned a
  blocking `TcpStream::connect` with no cap; a guest RST or device reset
  could not interrupt a blocked connect or write, and reset left the
  backend running. Concurrent connections are now capped at 64 (excess
  SYNs get a RST), connects carry a 10 s timeout and socket writes a 5 s
  timeout, and device reset tears the backend down (all connections, DNS
  state, queued frames), so every teardown path -- RST, connect failure,
  retransmit exhaustion, reset -- reclaims its thread and socket. Tests:
  `connection_creation_is_capped_and_excess_syns_get_a_reset`,
  `a_connect_to_a_black_hole_fails_within_the_timeout`,
  `a_reset_drops_every_connection_and_reclaims_the_host_socket`,
  `a_device_reset_discards_stale_backend_frames`.

### P2 fixes

- **Host burst over the pending cap reset the connection** (the review's
  alternative explanation for the intermittent `apk update` failures):
  the cap is now a backpressure point -- draining stops, the bounded
  channel stalls the host thread, and the host TCP stack shrinks its
  window -- never a mid-transfer reset. Test:
  `a_host_burst_over_the_pending_cap_applies_backpressure_not_a_reset`.
- **ACKs outside the receive window** `[our_una, our_next]` are ignored
  (an ACK past `our_next` used to be clamped and confirm data the guest
  never received). Test: `an_ack_outside_the_receive_window_is_ignored`.
- **Sequence wrap turning retransmitted data into SYN-ACK**: the SYN flag
  comes from the segment record, not sequence equality with `our_isn`.
  Test: `retransmitted_data_never_reuses_the_syn_flag`.
- **Retransmit exhaustion now sends the RST it counts**, instead of closing
  silently and leaving the guest to time out (~6.75 s of silence under
  loss). Test: `retransmit_exhaustion_sends_the_rst_it_counts`.
- **payload+FIN sequence accounting**: the FIN lands at `seq + payload_len`,
  so a segment carrying both completes the close instead of being read as a
  duplicate FIN. Test:
  `a_payload_and_fin_in_one_segment_advances_the_fin_sequence`.
- **UDP length and ICMP checksum**: a UDP datagram whose length field lies
  is dropped; ICMP echo requests must carry a valid ICMP checksum. Tests:
  `a_udp_datagram_with_a_lying_length_field_is_dropped`,
  `echo_replies_refuse_a_request_with_a_bad_checksum`.
- **Backend frames past the 1514-byte MTU** are dropped and counted on the
  receive path too (the cap was transmit-only). Test:
  `an_oversized_backend_frame_is_dropped_at_the_device`.
- **Oversized transmit completion lengths** saturate at u32::MAX instead of
  truncating a u128 total. Test:
  `an_oversized_transmit_chain_reports_a_saturated_length`.
- **Host clock before 1970** no longer collapses every ISN seed to a
  constant: seeds fold the wall clock and a per-process salt through an
  OS-entropy-keyed hasher. Test:
  `isn_seeds_stay_distinct_when_the_clock_reads_before_1970`.

### Regression record (this round, real runs)

`cargo test --workspace`: 615 passed / 0 failed / 3 ignored (baseline 589).

`RISH_NETWORK` unset, root disk built by `build-root-disk.sh`: `apk add
tree` from the baked file repo, `/dev/vda` mounted, and the marker read
back byte for byte.

`RISH_NETWORK=user-nat`, `/etc/apk/repositories` pointing at the https
mirror (main + community) from the root disk: gateway ping, then `apk
update` fetching both indexes over the backend (DNS + TLS + TCP), exit 0,
28,641 packages. Three more updates on a second boot: 3/3, exit 0.

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
