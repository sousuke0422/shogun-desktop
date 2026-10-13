# anim-shift-probe

Finds frames where an animated region of a terminal window (rt) has moved
away from where it was in the first frame, as an integer pixel offset
`(dx, dy)`. It only measures. It does not touch rt.

Two halves, so tests and re-analysis never need a live window:

| | runs on | does |
|---|---|---|
| `capture` | Windows | one PNG per window update, with a QPC timestamp |
| `analyze` | any OS | judges a saved PNG sequence and writes CSV, a summary and pictures |

```
anim-shift-probe capture (--title SUBSTR | --hwnd HWND) --duration-ms N --out DIR
anim-shift-probe analyze --in DIR --roi x,y,w,h [--mask x,y,w,h]... [--radius 8]
                         [--min-gain 1.0] [--min-gain-ratio 0.8] [--base-index 0] --out DIR
```

The crate is a workspace member, so `cargo test --workspace` and clippy cover
it. It is not in any release artifact, because the root
`cargo build --release` builds only the default member. Build it with
`cargo build --release -p anim-shift-probe`.

## capture

It uses **Windows.Graphics.Capture** (WGC) on one window. `FrameArrived`
fires as DWM composes the window. Measured on a static window, that is every
vsync (16.7 ms at 60 Hz), with identical pixels each time. So a frame
identical to the last kept one is dropped, and what remains is one PNG per
change of the client area. There is no fixed sampling clock. A fixed clock
beats against the app's paint clock and turns into false differences.

Each kept frame keeps `SystemRelativeTime`, the QPC time DWM composed the
first frame that showed the change. `frames.csv` stores it as milliseconds
since the first frame.

Desktop Duplication was rejected for three reasons:

- It captures a whole monitor, so updates from other windows land in the stream.
- It needs the window's screen position kept in sync.
- It cannot see a covered window.

Other details:

- The cursor is left out of the capture where the OS supports that.
- PNG encoding runs on worker threads, so the frame callback only copies.
- `capture.txt` records the HWND, title, client size, DPI, frame count and
  the number of identical compositions dropped.

Output: `frame_NNNNN.png`, `frames.csv` (`index,t_ms,file`), `capture.txt`.

## Coordinates and DPI

The ROI and masks are **window client rectangles in physical pixels**, with
(0,0) at the client area's top left. The captured frame is cropped to the
client area before it is saved, so these are also the pixel coordinates of
the saved PNGs. Read them straight off `frame_00000.png`.

The probe declares per-monitor DPI awareness (v2) before it measures the
window. As a result `GetClientRect` returns physical pixels, the same pixels
WGC delivers. At 150 % scaling a 10-pixel-wide cell is 15 pixels in the
PNGs, and the ROI must be given in those 15-pixel units. `capture.txt`
records the DPI.

## The judgement (analyze)

- The base is the ROI of the first frame, or of `--base-index`. Frames
  before the base are skipped.
- Each frame is compared in luma (Rec. 601).
- The search tries every integer offset within ±R (default 8 px) and keeps
  the `(dx, dy)` with the lowest mean absolute difference:

  ```
  mean over unmasked ROI pixels p of |base(p) − frame(p + (dx, dy))|
  ```

  `dx > 0` means right and `dy > 0` means down. Masks exclude pixels by their
  position in the base frame. Ties go to the smaller offset, so `(0,0)` wins
  a draw.
- A frame is **shifted** only when all of these hold:
  - the best offset is not `(0,0)`;
  - `diff(0,0) − diff(best) ≥ --min-gain` (default **1.0**, on a 0–255 scale);
  - `diff(0,0) − diff(best) ≥ --min-gain-ratio × diff(0,0)` (default **0.8**).

Why these defaults:

- **min-gain 1.0.** GPU output is deterministic, so unchanged content
  differs by exactly 0. Anything under about 1 is a few anti-aliased pixels.
- **min-gain-ratio 0.8.** A real translation of the ROI lines up exactly and
  explains nearly all of the `(0,0)` difference. In the tests this was 0.9 or
  more, even with the spinner turning at the same time.

  A glyph swap is only partly explained by moving. Braille spinner frames
  (⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏) are dot patterns that *are* translations of one another. An
  unmasked spinner alone in an empty ROI got up to 0.667 explained, and at
  0.5 six of twenty frames came back as false drifts. 0.8 keeps those out.

  The cost: if only part of the ROI moves, less of the difference is
  explained, and the frame may be missed. Put the ROI on the region that
  moves, and mask the spinner cell when you can.
- **unexplained** is a separate mark. Something outside the masks changed
  (`diff(0,0) ≥ --min-gain`), but no offset within ±R explains it. This is
  either a change that is not a drift, or a drift larger than R over sharp
  text. For sharp text the cost does not fall toward the boundary, so
  `at_edge` stays silent. A one-row drift at 150 % is 24 px. Measured live,
  it came out as unexplained at R=8 and as `(0,24)` with best diff 0 at
  R=30.

  With the masks covering everything that may change, this should be 0. If
  it is not, raise `--radius` or widen the masks.
- **at_edge** is a separate mark. It means the best offset sits on the ±R
  boundary, so the content may have moved further than the search covers.
  A drift beyond R is only partly explained at R and can miss the
  thresholds. Rerun with a larger `--radius`.

## Output (analyze)

- `shift.csv`: `index,t_ms,dx,dy,diff0,best_diff,shifted,at_edge,unexplained`.
- `summary.txt`: frame count, shifted frames, frames at the search edge,
  frames with an unexplained change,
  the largest dx and dy, and the first shifted frame and its time.
- For each shifted, at-edge or unexplained frame:
  - `shift_NNNNN_side.png`: the first frame's ROI, a magenta bar, then this
    frame's ROI;
  - `shift_NNNNN_diff.png`: `|base − frame|` at `(0,0)`, ×4 in red, with
    masked pixels in blue.

## Live runs

See `e2e/anim-shift/` and the section in `e2e/README.md`. A recording is
replayed inside rt with `scriptreplay` while `capture` runs.
