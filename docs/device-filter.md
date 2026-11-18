# Device filters

Standalone processing contract; cell arrangement → [Devices grid](device-grid.md). Applies to device graphs only; marker Smooth algorithm unchanged.

## Chain and input

- One ordered chain per file/device; all channels, grid cells, views/tabs and floating plate graph share it. Initial empty/bypassed, Show raw enabled. Repeat/reorder/remove stages freely; committed edits calculate immediately without Apply. Display only: no file values, dirty state, saves or undo history changed; chains never persist in app.ron.
- Stages: Demean, Low-pass, High-pass, Band-pass, Band-stop, Rectify, Moving RMS. Invalid fields retain last valid output with short English reason; pending computation retains previous curves. Visible explanation: `Display only · file values unchanged`.
- Orchestrator choice, unconfirmed defaults on stage creation only: LP6 Hz, HP20 Hz, BP20–450 Hz, BS59–61 Hz, order4, Zero-lag enabled, RMS50 ms. Start below Nyquist by clamping new defaults; entered values never silently corrected.
- Plate input = recorded frame means at FrameRate. Other analog = every valid sample at samples/frame×FrameRate. Structural proof of sample count required for high-frequency processing; never guess EMG physiology from device names. 3D floor colors remain raw.
- Nonplate output reduced to per-frame mean plus pale min/max envelope. Signed raw input also retains within-frame extrema; mean cancellation must not erase amplitude. Raw overlay behind processed curve, same color at lower opacity.

## Numeric definition

- Pure Rust f64 Butterworth, order1–8, bilinear transform/prewarp, SOS cascade. LP/HP design N; BP concatenates HP(low)→LP(high); BS transforms lowpass prototype, digital order2N. Core also offers IIR notch(f0,Q). Input Hz is design cutoff, no forward/backward frequency correction.
- Zero-lag splits finite runs, odd-reflects boundaries and filters forward/backward. padlen=3×actual transfer order: LP/HP3N, BP/BS6N. Initialize each direction with steady-state SOS values, cumulative section DC gain as `sosfilt_zi`. Disabled Zero-lag = zero-state causal single pass.
- Compare scipy `sosfiltfilt` with explicit padlen; its current default differs with SOS count/odd-order correction. References: [sosfiltfilt](https://docs.scipy.org/doc/scipy/reference/generated/scipy.signal.sosfiltfilt.html), [iirnotch](https://docs.scipy.org/doc/scipy/reference/generated/scipy.signal.iirnotch.html). Required relative-scaled absolute error ≤1e-9: abs(Rust−scipy)/(1+abs(scipy)).
- Demean subtracts finite full-record mean; Rectify takes absolute value. RMS window=round(fs×ms/1000), minimum1; centered, even length includes one extra left neighbor. Edge divisor = actual sample count within same finite run.
- NaN/Inf separate runs; never join across gaps. Any run too short for any stage's padding remains raw for entire chain; report skipped count. Require 0<cutoff<fs/2, low<high, 1≤N≤8.

## Workers and validity

Result key = file/device/raw generation/chain. Accept current results only; a completed plate is not recomputed when other analog arrives. Keep four completed chains/device, retire oldest large buffers off-thread. Reading prepares full samples/rates; filtering and disposal stay on workers. Hidden tabs start no fresh reads. Product never starts local Python for processing.

## Steady controls

- Computing occupies fixed-width header-right slot; blank when idle. Each stage reserves one error line; worker error and short-run count each reserve fixed footer line. Values/status never shift existing rows. Invalid field stays red; last valid output retained.
- Numeric values/order use typed text boxes. Enter/blur commits; Esc restores prior value. Invalid text stays visible/red until corrected; no drag-to-change. Other chain behavior unchanged.
