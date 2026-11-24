# Context retention reduced cumulative request bytes by 72% on 8 KiB reads

Measured through the `rof run` CLI at `97075cf` with a scripted local
chat-completions provider. Binary SHA-256
`5c416117d2a041343cd8d3cf4840f3e861180063c74a398581ea74e854657507`.

## Request payload

Each arm performed the same 40 successful file reads, followed by a final
response. Only observation retention changed. Keep 128 retained all 40
observations. Counts are bytes of serialized HTTP request bodies, before
transport compression, including the tool schema and message history.

| File size | Keep all, cumulative | Keep 5, cumulative | Keep all, final | Keep 5, final |
|---|---:|---:|---:|---:|
| 1 KiB | 1,189,469 | 635,617 | 54,447 | 23,680 |
| 8 KiB | 7,009,829 | 1,984,237 | 338,367 | 59,170 |

## Archive and rollback

- With `--archive-output on`, read 1,048,576 ASCII bytes via `exec`, then
  advance the context window with six tool results. The initial tool result
  was 8,103 bytes including metadata. The collapsed result was
  159 bytes including the handle. `output_read` returned all bytes in
  32 pages with the same SHA-256. The 64 KiB fixture also matched exactly.
  With archival off, both fixtures produced previews without archive handles.
- Four fixtures each for command denial, missing-file read, unmatched edit
  and nonzero command exit. In all 16 trials the earlier edit survived, the
  overwritten file matched its original bytes, the failed batch's new file
  was absent, and the rollback was recorded in the log.

These checks measure request size and tool behavior. Model token usage, cost,
latency and solve-rate effects remain unmeasured by these fixtures.
