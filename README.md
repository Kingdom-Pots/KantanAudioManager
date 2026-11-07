# Kantan Audio Manager

A lightweight Windows audio editor for recording, editing and batch exporting voicelines. Everything sits on one track, and each clip exports as its own file, named from the clip.

- Record from a microphone or system audio with one button (`R`)
- Split, merge, rename, reorder and amplify clips, with full undo and redo
- Optionally split takes into one clip per line at pauses
- Export to WAV, MP3 or OGG with prefix, suffix, numbering and a live filename preview
- Save projects as a single `.kantan` file, with autosave and crash recovery

## Build

You need [Rust](https://rustup.rs) and Visual Studio's C++ build tools.

- `run.bat` builds and opens the app.
- `build-release.bat` writes the optimized app to `dist\`. It needs Python to find the build folder.
- `cargo test --features bridge` runs all the tests. `CLAUDE.md` explains the headless UI driver they use.

## Licence

MIT, copyright 2026 Kingdom Pots. See [LICENSE](LICENSE). The third-party components and their licences are listed in [THIRD_PARTY_NOTICES.txt](THIRD_PARTY_NOTICES.txt). After changing dependencies, regenerate it with `python scripts/generate_notices.py`.
