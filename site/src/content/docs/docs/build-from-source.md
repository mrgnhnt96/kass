---
title: Build from source
description: Build and install Kass yourself, and keep permissions across updates.
---

Building Kass yourself is for working on it. To just use Kass, [download the DMG](/docs/install/).

## Requirements

- An Apple Silicon Mac
- [Xcode](https://developer.apple.com/xcode/)
- [Rust](https://rustup.rs) and [Python 3.12](https://python.org)

## Build and install

```bash
git clone https://github.com/mrgnhnt96/kass.git
cd kass
./scripts/install.sh
```

The script checks for everything the build needs and prints how to install anything missing. It then builds the app and installs it to `/Applications/Kass.app`. Run it again to pull the latest code and update. A first build needs about 15 GB of free space.

Every build is signed with the same identity, so updates keep your Microphone, Accessibility and Input Monitoring permissions. That identity is your Apple Development certificate if you have one. Otherwise the first install creates a self-signed "Kass Local Signing" certificate, and macOS asks for your password once to trust it.

## Develop

```bash
cargo xtask setup   # Python environment, dependencies and the pre-push hook
cargo xtask dev     # runs the model worker and the desktop app
```

Run the commands from the repo root. `cargo xtask --help` lists every command.

`git push` lints and formats the files it pushes, and only those. If it fixes anything, it stops the push and lists the files: commit them and push again. `SKIP=1 git push` skips it once.

| Part | Built with |
| --- | --- |
| Desktop app and interface | Rust, with GPUI |
| Hotkeys, audio, text insertion | Native Rust |
| Local server | FastAPI (Python), bundled with the app |
| Speech and cleanup | Whisper and Qwen3 on MLX |
| Storage | SQLite |
