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
process-group cleanup, and reaping. The binary polls every 10 ms even without
inbound control frames, so a long command cannot block `Ping`, `Stream`, or
`Cancel`.

Resource use is bounded:

- at most four bootstrap executions are active by default (hard ceiling 16);
- each stream chunk is at most 32 KiB;
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

User switching, OCI lifecycle dispatch, container attach, and other privileged
handlers remain unavailable in this bootstrap implementation and fail closed.
PTY support is deliberately Linux-only; other guest targets reject `tty:
true`.

## OCI runtime component

`OciRuntimeBackend` is a bounded blocking lifecycle component for a future
full guest handler. It invokes one configured absolute `youki` or `runc`
executable directly, without a shell, and implements the OCI
`create`/`start`/`state`/`kill`/`delete` command flow. Container identifiers,
digests, bundle paths, rootfs paths, runtime output, OCI spec size, signals,
timeouts, forced cleanup, and atomic `config.json` publication are validated.

The bootstrap agent does not advertise or dispatch this component yet.
Lifecycle commands must run on a worker so they cannot block protocol polling,
and capability probes must prove that the guest kernel and runtime are ready
before `container.oci` becomes available. The bundle must already contain a
full-fidelity rootfs on a guest Linux filesystem. The mobile CAS and portable
snapshot cannot be used directly: a separate host-to-guest import path must
transfer the verified descriptors and layers, recheck their digests and
diff-IDs in the guest, preserve Linux metadata, and materialize `rootfs`.
After those checks, that trusted importer must publish the bounded,
versioned `.rish-verified-rootfs.json` record that binds the bundle to the
resolved image digest; OCI prepare fails closed without a matching record.

Interactive `attach` and terminal OCI specs deliberately fail closed until the
protocol has an OCI-exec/console operation with PTY streaming, resize, detach,
and reconnect semantics.
