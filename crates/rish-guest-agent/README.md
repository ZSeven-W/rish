# rish guest agent

This bootstrap agent is the responsive control plane inside a rish Linux
guest. It is intentionally not a container runtime or a claim of host-kernel
privileges.

`Exec` creates a supervised child and returns an `ExecStarted` response
immediately. Non-TTY executions use isolated pipes and may attach stdin,
stdout, and stderr independently. On Linux, `tty: true` allocates a real
pseudoterminal, creates a new session with that controlling terminal, and
relays merged output on the `console` stream. `Stream` requests write or close
stdin and resize an owned PTY by exact `execution_id`.

`GuestAgent::poll` advances nonblocking input/output, deadlines, cancellation,
process-group cleanup, and reaping. The binary polls every 1 ms while idle and
continuously services the UART while outbound bytes are queued, so a long
command cannot block `Ping`, `Stream`, or `Cancel`.

Resource use is bounded:

- at most four bootstrap executions are active by default (hard ceiling 16);
- each stream chunk is at most 1 KiB so UART consumers receive bounded frames
  promptly;
- pending stdin is bounded to four stream chunks per execution;
- stdout and stderr each relay at most 4 MiB by default;
- a merged PTY console relays at most the combined stdout/stderr allowance,
  capped at 64 MiB;
- the control reader has one thread and a two-chunk synchronous queue;
- process I/O uses nonblocking pipes/PTYs and transport backpressure, not
  per-process reader threads or an unbounded event queue.

Children never inherit the framed control stream. A non-attached stdin is
always null; attached stdin is an agent-owned pipe or PTY endpoint. Closing a
pipe drops it after queued bytes drain; closing a PTY sends the terminal EOT
character after queued input. Cancel and timeout kill the process group and the
supervisor reaps the leader. When the leader exits, remaining group members are
killed. Readers are forcibly closed after a 250 ms drain grace period, so an
escaped descendant that changed session/process group cannot hold the control
session open forever.

User switching, container attach, and other privileged handlers remain
unavailable and fail closed. PTY support is deliberately Linux-only; other
guest targets reject `tty: true`.

## OCI runtime component

`OciRuntimeBackend` is the bounded lifecycle component used by the guest
handler when the live probe finds a safe absolute `youki` or `runc`
executable. It invokes that executable directly, without a shell, and implements the OCI
`create`/`start`/`state`/`kill`/`delete` command flow. Container identifiers,
digests, bundle paths, rootfs paths, runtime output, OCI spec size, signals,
timeouts, forced cleanup, and atomic `config.json` publication are validated.

`bootstrap_guest_agent` probes the running kernel and publishes
`container.oci` only when all of these are observed: uid 0, cgroup v2, the
required Linux namespace handles, and a non-group/world-writable executable
runtime. `systemd` is reported only when PID 1 is systemd and
`/run/systemd/system` exists. The ordinary `bootstrap_agent` constructor stays
conservative and does not probe the host filesystem.

The current guest control loop invokes OCI lifecycle calls through a bounded
synchronous handler; the existing non-blocking responsiveness guarantee applies
to `Exec`, `Stream`, and `Cancel`, not to an in-flight OCI call. A production
transport still needs a worker/async response boundary before it can promise
that `Ping` remains responsive during lifecycle operations. The bundle must
already contain a full-fidelity rootfs on a guest Linux filesystem. The mobile
CAS and portable snapshot cannot be used directly: a separate host-to-guest
import path must transfer the verified descriptors and layers, recheck their
digests and diff-IDs in the guest, preserve Linux metadata, and materialize
`rootfs`. After those checks, that trusted importer must publish the bounded,
versioned `.rish-verified-rootfs.json` record that binds the bundle to the
resolved image digest; OCI prepare fails closed without a matching record.

Interactive `attach` and terminal OCI specs deliberately fail closed until the
protocol has an OCI-exec/console operation with PTY streaming, resize, detach,
and reconnect semantics.
