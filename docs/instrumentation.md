<!-- SPDX-License-Identifier: MIT-0 -->
<!-- Copyright (C) 2026 Simplebooks Foundation -->
<!-- Copyright (C) 2026 Josh Rodd -->

# Instrumentation and vision handoff

## Choosing the evidence source

- Use `replay` for terminal applications whose text and styles are the evidence. It is not bitmap OCR.
- Use `analyze` for raw output that paints sextant graphics in a 320×80 terminal.
- Use `image` for an already available, unscaled 160×200, 320×200, or 640×200 PNG and a caller-supplied 8×8 font.
- Use `capture` when the tool should own and bound a new child process. Choose text or bitmap mode before launching it.

Existing transcripts can be ordinary files, a producer's stdout pipe, or standard input using `--capture -`. PNG files and binary streams use `--input -`. Keep binary streams separate from human-readable logs: merged logs can corrupt a PNG or paint unintended text into a terminal transcript. These modes consume an input stream; they are not an interactive terminal attachment or a live vision service.

## Owned PTY lifecycle

On macOS and Linux, `capture` launches the explicit argv in a newly owned session/process group with a 320-column × 80-row PTY. It collects raw PTY output into `PREFIX.pty`, bounded by a 10-second default deadline and a 64 MiB default byte cap. `--timeout-ms` and `--max-bytes` choose other bounds. Terminal line discipline may transform what the child writes; the saved bytes are the raw PTY output observed by the collector, not a promise to match a child's pre-terminal write buffer.

The child receives `TERM=xterm-256color` and `COLORTERM=truecolor`. Its stdin is attached to the PTY, but the collector does not forward interactive input. Reaching the byte cap is inclusive: a zero cap stops immediately, and exactly filling a nonzero cap counts as reaching the limit. After the leader exits, the collector drains only immediately available output for a bounded interval, then cleans up remaining members of its owned group. Output emitted during termination is not promised in the transcript.

The tool cleans up and reaps its own child on timeout, byte limit, or capture error. Signals target only its newly owned child/session group; it does not search for other instances, attach to existing sessions, or kill unrelated programs. The raw transcript is retained on errors where it could be created. Lifecycle outcomes are written separately to `PREFIX.capture.json`; partial output can still yield useful replay or analysis evidence. Process-spawn or filesystem failure can prevent some artifacts from being written, so always inspect the exit status as well.

The PTY size is independent of the caller's terminal size and bitmap resolution. Only explicitly requested shell executables interpret shell syntax. For example, `-- /bin/sh -c '...'` requests a shell, while `-- program '*.png'` passes the literal glob argument to `program`.

Capture is **not a sandbox**: the child runs with the caller's privileges and can access the caller's files and network. Limits bound collection time and transcript size, not child memory, CPU, filesystem changes, or adversarial descendants that detach from the owned group. Do not run untrusted commands merely because output is bounded. Avoid secrets in argv, terminal output, transcripts, and JSON. Raw transcripts may contain terminal controls; inspect them as data rather than blindly replaying them into a trusted terminal.

## Snapshot and color interpretation

When an application leaves the alternate screen during shutdown, the last alternate snapshot is selected rather than the restored shell prompt. An active alternate screen is selected while still present; otherwise the primary screen is used. Consult `terminal_snapshot` when comparing a result to a capture. This is a snapshot, not a video timeline, and late primary-screen output may not be the selected evidence.

The model records style attributes and effective RGB values. Default and palette colors approximate a host terminal; they do not capture host font rendering, palette customizations, blink timing, or screen pixels. Bitmap reconstruction knows sextant masks and their colors, not arbitrary terminal glyph rasterization. Missing and invalid pixels remain transparent. Do not flatten transparency against black and then interpret that as measured background evidence.

## Hand off to a person or vision-capable tool

No vision service, model account, or network integration is required. The outputs are portable files:

1. Pass the cropped `.png` for visual inspection, preserving its alpha channel and exact pixel dimensions.
2. Include `.json` for region coordinates, coverage, colors, glyph alternatives, ambiguity, confidence, and diagnostics. A crop's `(0,0)` corresponds to the report's region origin in the full frame.
3. Include `.txt` as a convenience, not ground truth. `?` is unresolved evidence; an empty file with recognition disabled does not mean an empty screen.
4. For image input, include `.original.png` if the consumer needs the unmodified source. For captured children, include `.capture.json` and, when safe, `.pty` to explain lifecycle/truncation or reproduce analysis.

A useful request is: “Inspect this exact-grid PNG with its JSON evidence. Distinguish observed pixels from transparent unknowns, preserve ambiguous candidates, and do not assume the expected string was independently recognized.” External interpretation should not replace the tool's exact mask diagnostics or convert a nearest candidate into a claimed exact match.
