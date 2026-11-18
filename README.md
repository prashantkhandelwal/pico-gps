# Pico GPS desktop app

Run `cargo run` from this directory. Enter `http://<pico-ip>/data` using
the address printed by the Pico, then click **Connect**.

The egui app polls every 500 ms without blocking the UI. HTTP requests time
out after five seconds; failures are retried automatically.
The connection circle is green after a successful response (even with no GPS
data), and red before connecting, after failures, or when disconnected.
Hover over it for connection status and error details.
Disconnect stops polling and leaves the last received values visible.

The Pico serves raw NMEA text, not JSON. The app assembles sentences across
responses, checks checksums when supplied, and shows every field in a disabled
textbox. Empty fields remain empty. Common GPS sentences have descriptive
labels; unfamiliar sentences and additional fields use numbered labels.
Values retain their original NMEA format (including coordinates, UTC dates,
times, and status codes). Raw sentences are also displayed.
Fields whose values change briefly get an amber background for two seconds.
The first received snapshot is not highlighted; repeated unchanged values do
not restart the highlight.
Sentence sections use equal-width, responsive columns with compact spacing
(4 pixels horizontally and 2 vertically). Each section goes into the shortest
column, filling gaps beneath shorter sections instead of waiting for a tall row.
The upper section area fits its content and scrolls when necessary, reserving
space below for the raw stream on the left and converted coordinates on the
right. This lower area automatically fills the remaining height rather than
using a fixed-height, manually resized split.
Section headings are plain labels; their contents are always shown.

The position panel converts the latest GGA, GLL, or RMC position from DDM to
signed decimal degrees, displayed to six decimal places. South and west are
negative. Original fields remain in NMEA format. Missing or invalid fixes clear
the converted values rather than displaying zero or an old position; malformed
coordinates display an error. The source sentence is shown below the values.

GSV pages are combined in one compact, scrollable satellite section, grouped by
talker and signal ID. Each row shows page, satellite ID, elevation (degrees),
azimuth (degrees), and SNR (dB-Hz). Page counts and satellites in view remain
visible. When a group's page count shrinks, obsolete higher pages are removed.

The read-only raw stream auto-scrolls to the latest data unless you scroll up.
It preserves received text, including incomplete or invalid sentences, and keeps
the latest 20,000 bytes (trimmed at a UTF-8 character boundary). Reconnecting
clears both the parsed values and raw history; disconnecting preserves them.

The latest sentence of each type and talker is retained. GSV satellite pages
and signal IDs and GSA system IDs are retained separately. Parse errors remain
in the circle's tooltip until reconnecting; invalid sentences do not replace
valid values.

Only one client should poll `/data` at a time: the Pico drains its shared GPS
buffer on each successful response. Close its browser page while using this app.

Validation: `cargo test`, `cargo clippy --all-targets -- -D warnings`.
