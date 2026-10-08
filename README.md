<p align="center">
  <img src="docs/assets/icon-dark.webp" alt="Kass" width="120" height="120" />
</p>

<h1 align="center">Kass</h1>

<p align="center">
  <strong>Private dictation for your Mac.</strong><br/>
  Hold a key, speak, and let go. Kass turns what you said into clean, ready-to-send text in any app.<br/>
  Everything runs on your Mac.
</p>

<p align="center">
  <a href="https://kass.mrgnhnt.com">Website</a> ·
  <a href="https://github.com/mrgnhnt96/kass/releases/latest">Download</a> ·
  <a href="https://kass.mrgnhnt.com/docs/">Docs</a> ·
  <a href="https://kass.mrgnhnt.com/changelog/">Changelog</a>
</p>

<p align="center">
  <img src="docs/assets/readme/captures.png" alt="Kass's Captures tab: what you said next to the cleaned-up text" />
</p>

## How it works

Hold the chord in any app and talk the way you think. Whisper transcribes while you speak, and a local LLM drops the ums, applies your "no, actually"s and pastes the result into the field you started in. It never summarizes or adds words you didn't say.

## Features

- **Writing styles.** Each app gets its own style that learns how you write.
- **Dictionary.** Names and jargon, spelled the way you want.
- **Command Mode.** Select text anywhere and say how to rewrite it.
- **Correction learning.** Fix a result once and Kass learns from it.
- **Captures.** Every take is kept with its audio, so you can replay, re-transcribe or fix it.
- **Private.** No account and no server. Audio, text and models stay on your Mac. Kass sends only anonymous daily usage counts, never your words, and you can turn them off ([what's sent](https://kass.mrgnhnt.com/docs/privacy/#usage-stats)).

<p align="center">
  <img src="docs/assets/readme/writing-styles.png" alt="Writing styles, one per app" />
</p>

<p align="center">
  <img src="docs/assets/readme/insights.png" alt="Insights: words dictated, speaking pace and time saved" />
</p>

## Install

Requires an Apple Silicon Mac. Download the latest DMG from [Releases](https://github.com/mrgnhnt96/kass/releases/latest). Kass updates itself in the background.

## Development

Built with Tauri (Rust), React and a bundled FastAPI server running Whisper and Qwen3 on MLX.

```bash
brew install just
just setup     # Python venv, dependencies and the pre-push hook
just dev       # backend + desktop app
just check     # lint, format and typecheck
just test      # backend tests
just install   # build and install to /Applications
```

You'll need [Bun](https://bun.sh), [Rust](https://rustup.rs), [Python 3.12](https://python.org), Xcode and the [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/). See [Build from source](https://kass.mrgnhnt.com/docs/build-from-source/) for signing and details.

`git push` runs a pre-push hook (`scripts/hooks/pre-push`) that lints and formats only the files the push changes. If it fixes anything, it stops the push and lists the files: commit them and push again. `SKIP=1 git push` skips it once.

To release, move the Unreleased notes in [CHANGELOG.md](CHANGELOG.md) under the new version, commit, and run `./scripts/release.sh <version>` from a clean `main`.

To try a feature before it's public, run `./scripts/release.sh 0.7.0-beta.1` from any clean branch. Betas are published as prereleases: the website and the public update skip them, and only copies with **Settings › General › Beta updates** on install them. Betas keep their notes under Unreleased until the public release.

To ship a feature in a public release without showing it yet, add its name to `BETA_FEATURES` in [app/src/lib/betaFeatures.ts](app/src/lib/betaFeatures.ts) and gate it with `useBetaFeature(name)` (the server has `beta.enabled(name)` in [backend/beta.py](backend/beta.py); Rust has `updater::beta_features`). It only shows for beta users. Remove the name to make it public; the compiler points at every gate to remove.

| Path       | What                                               |
| ---------- | -------------------------------------------------- |
| `app/`     | React frontend                                     |
| `tauri/`   | Desktop shell and native dictation code (Rust)     |
| `backend/` | Python server: STT, refinement, captures, learning |
| `site/`    | Website, docs and changelog                        |

## License

MIT. Kass started as a fork of [Voicebox](https://github.com/jamiepine/voicebox) by Jamie Pine. It used to be called Herga. The name comes from Kass, the bard in *Breath of the Wild* who carries songs from place to place.
