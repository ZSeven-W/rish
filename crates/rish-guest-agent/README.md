# rish guest agent

This bootstrap agent is the responsive control plane inside a rish Linux
guest. It is intentionally not a container runtime or a claim of host-kernel
privileges.

`Exec` creates a non-interactive child in its own process group and returns an
`ExecStarted` response immediately. `GuestAgent::poll` advances nonblocking
stdout/stderr reads, deadlines, cancellation, process-group cleanup, and
reaping. The binary polls every 10 ms even without inbound control frames, so a
long command cannot block `Ping` or `Cancel`.

Resource use is bounded:

- at most four bootstrap executions are active by default (hard ceiling 16);
- each stream chunk is at most 32 KiB;
- stdout and stderr each relay at most 4 MiB by default;
- the control reader has one thread and a two-chunk synchronous queue;
- process output uses nonblocking pipes and transport backpressure, not reader
  threads or an unbounded event queue.

Children receive a null stdin and piped/null output descriptors, never the
framed control stream. Cancel and timeout kill the process group and the
supervisor reaps the leader. When the leader exits, remaining group members are
killed. Pipe readers are forcibly closed after a 250 ms drain grace period, so
an escaped descendant that changed session/process group cannot hold the
control session open forever.

TTY, streamed stdin, user switching, OCI lifecycle, and other privileged
handlers remain unavailable in this bootstrap implementation and fail closed.
