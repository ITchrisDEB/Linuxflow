# Performance and memory measurements

Real measurements, taken on 2026-10-25 with the installed archive (`cargo xtask dist`), on ONE machine:
a recent 16-core desktop CPU (32 threads), 30 GB of RAM, a recent NVIDIA discrete GPU (Vulkan driver), KDE Plasma.
Protocol: `linuxflow --test-pipeline` on 10 s of French speech (an excerpt of a test sentence read by a
synthetic voice), one model loaded at a time, a single run per row (no averaging: these are orders of
magnitude, not a guarantee).

| Device | Model (file) | Load time | Transcription of 10 s of audio | Memory |
|---|---|---|---|---|
| Vulkan GPU (recent NVIDIA) | `large-v3` quantised q5_0 (1.08 GB) | 0.5 s | **0.88 s** | process RAM ≈ 270 MB; VRAM ≈ 1.8 GB (model 1,080 MB + compute buffers and cache ≈ 740 MB, read from the whisper.cpp log) |
| CPU (16 cores) | `small` f16 (488 MB) | 1.2 s | **2.3 s** | process RAM ≈ 890 MB |

Goal (P1): "10 s of audio in under 2 s on a GPU" → **met** on this GPU (0.88 s).

## What these figures do not tell
- One graphics card (NVIDIA) and a very powerful processor. AMD and Intel GPUs, modest processors and
  the fallback engine for CPUs without AVX2 (about 6.5 times slower than the normal engine, measured
  during development) were not measured here: fill them in with the checklist in `docs/manual-validation.md`.
- Unquantised `large-v3` (3.1 GB) was not timed; it needs about 3.5 GB of VRAM.
- The first dictation after launch includes the model load time; the following ones do not pay it.

## Test coverage (2026-10-25)
Measured with `cargo llvm-cov --workspace` (rustup `llvm-tools` component), tests and examples excluded.

| Scope | Regions | Lines |
|---|---|---|
| Core (`linuxflow-core`) | 90 to 100 % per module, ≈ 97 % on average | ≈ 97 % |
| Whole workspace | 82.6 % | 81.9 % |

Goal: "core coverage ≥ 85 %" → **met**. The rest of the workspace is less covered because some modules
need a real desktop session: shortcut portal (0 %), X11 shortcuts (0 %), evdev (0 %), X11 and Wayland
clipboards (0 to 25 %), portal pasting, virtual keyboard and uinput (30 to 50 %), the real Whisper
engine (38 %). They are covered by the manual checklist (`docs/manual-validation.md`), not by
automated tests.
