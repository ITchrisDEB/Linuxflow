# Design decisions

Choices made while building the project, with the reason for each. Any of them can be revisited.

| # | Decision | Reason | Where |
|---|---|---|---|
| 1 | The Whisper engine runs in a **child process** of the same binary (`--engine-worker`), restarted at most twice | A GGML_ASSERT/SIGABRT/SIGSEGV kills the process and cannot be caught (measured: exit 134/139 under memory pressure) | crates/linuxflow-stt |
| 2 | The recording pill runs in a **child process under XWayland** on GNOME | GNOME has no layer-shell; this avoids forcing the whole application onto X11 | crates/linuxflow-gtk/src/overlay |
| 3 | A snippet expansion is inserted **literally** (no interpretation of `$&`, `$$`) | Interpreting those sequences would be accidental; the literal form is safer | crates/linuxflow-core/src/format.rs |
| 4 | Vocabulary token bug fixed (indexes use private-use characters U+E010–E019) | Digits could trigger numeric rules ("zero") | crates/linuxflow-core/src/vocab.rs |
| 5 | **No gettext-rs**: pure-Rust `.mo` reader | libintl is global state, LANGUAGE is ignored if the locale is not generated, C dependency | crates/linuxflow-i18n |
| 6 | Distribution build at **x86-64-v3** (`GGML_NATIVE=OFF`) + a v2 fallback engine for CPUs without AVX2 | native = AVX-512 (SIGILL elsewhere); v3 = same speed; v2 = 6.5x slower | .cargo/config.toml |
| 7 | VAD: Silero applied BEFORE transcription (the built-in VAD of whisper-rs 0.16 does not work) | Measured: silence leads to hallucinated text without a filter | crates/linuxflow-stt |
| 8 | A model with no known checksum is "unverified" (not refused) | It stays usable, with a visible warning | crates/linuxflow-models |
| 9 | evdev/uinput disabled by default | Equivalent to reading the keyboard: an explicit advanced option | crates/linuxflow-platform |
| 10 | Noise suppression/AGC: **not added** | Measured: it makes transcription worse; reserved for the maintainer | n/a |
