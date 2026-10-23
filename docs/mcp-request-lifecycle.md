# MCP request lifecycle and output retention

This note describes the lifecycle implemented by the MCP request-ownership change. It records
its existing policy for architecture review; it does not establish Windows test acceptance.
The implementation lives in `crates/automation/src/{server,request_lifecycle,stdio_input,long_job}.rs`.

## Ownership and shutdown

Each `serve_io` transport has an independent admission scope, even when service clones share
an engine session. Admitted tool calls and resource reads retain request permits. Started
headless blocking commands and renders retain separate work permits inside their closures, so
cancelling an awaiting request does not make that blocking work disappear from accounting.
Admission and retained work share a limit of 1,024 active operations; excess work returns an error.

| State | Behavior |
|---|---|
| Accepting | Admit requests and retain their started blocking work. |
| Transport stopping | EOF ends the service; output write/flush failure stops input and cancels the service. |
| Draining | Close admission, reject new requests, and wait for retained permits. Existing cleanup can retain blocking work. |
| Drained | All tracked requests and blocking work released their permits. |
| Unconfirmed | A drain deadline expired; retained state is not evidence of completion or permission to delete files. |

One guarded writer owns response output. Initialization, output failure and competing EOF paths
preserve an observed write/flush diagnostic. If service failure and cleanup failure coexist, the
returned diagnostic starts with the original error and includes labeled cleanup failures, even
without a logger. Single-error outcomes are unchanged. The output writer's final
join has a two-second budget; guards abort transport tasks when their owner drops.

Process stdin is bridged through 64 KiB of bounded storage on a dedicated input runtime. After
service cleanup, explicit input `finish` signals stop and waits up to one second for the outer
driver. Only that input runtime uses a 100 ms runtime shutdown timeout. A residual blocking OS
stdin read can survive until EOF or process exit; joining the outer driver does not prove the OS
read ended. Dropping the input owner signals stop but does not confirm driver completion.

`close_and_drain` waits up to **65 seconds** for tracked requests and started blocking work while
the main runtime remains available. Timeout reports `completion unconfirmed`; it neither clears
the permits nor proves workers stopped. This is a drain budget, not an end-to-end process deadline.
Externally dropping the entire service future is not equivalent to awaiting its orderly cleanup.

## Long exports and cancellation

For `file.exportMedia` with `wait: true`, MCP starts a background engine job and retains its job ID
before cancellation-aware polling. Progress is reported at most every 100 ms when a progress token
is supplied. Normal terminal success returns the result and retains output; terminal export failure
returns an error. A malformed or missing job observation remains an error rather than success.

On request cancellation, MCP asks `jobs.cancel` to stop the job and observes its terminal state.
A shared **60-second** deadline covers that cancellation command and every subsequent observation.
A query failure also enters this bounded cleanup path while retaining the original query diagnostic.
MCP cancellation normally suppresses the response; producing a handler error does not guarantee
that a cancelled client receives it. The job remains observable through `jobs.list` when available.

| Observation during cleanup | Outcome |
|---|---|
| Export succeeded, including a race with cancellation | Keep successful output; report cancellation after completion / successful output retained. |
| Export failed | Report that the job stopped but file ownership is unconfirmed; retain output. |
| Cancellation failed, job disappeared, state is malformed, or deadline expired | Report cleanup unconfirmed; retain output. |

The engine's job result does not prove which files that job exclusively owns. A path, matching
filename or modification time can describe another job's successful output. MCP therefore does
not invoke file deletion during this cleanup. Users may inspect retained partial files and remove
them manually after confirming they are no longer in use. Retention trades possible leftover
partial files for protection against deleting another export or an output still being written.
These deadlines do not authorize terminating native workers or establish their complete inventory.

## Verification boundaries

Portable automation tests cover synthetic stdin/response failures, admission/drain accounting,
cloned transports, blocking-work ownership, cancellation races and uncertain-output preservation.
They do not exercise an actual process's OS stdin shutdown.

The Windows-only `filmcraft-cli` integration target `mcp_output_lifecycle` launches the actual CLI,
closes its output during or after initialization, and keeps its input open. Its assertions require
parent exit status 1 with an output diagnostic within fixture deadlines. These tests prove that
parent process behavior, not that every native worker or descendant was inventoried or stopped.
Run both scopes on the final source head; Linux skips the Windows-only process tests. Commands
and further evidence limits are in [testing.md](testing.md#mcp-response-failures-and-shutdown-ownership).
