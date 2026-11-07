# Page brief

- Type and state: desktop app site, live. Home, `/download/`, `/changelog/`, and the docs at `/docs/` (Starlight, not marketing pages, but `/docs/privacy/` carries the privacy claims).
- Audience: people on Apple Silicon Macs who write all day and already know dictation exists, but find macOS dictation or cloud tools too raw or too exposed.
- The page's one job: get that person to download Kass.
- Primary CTA: "Download for Mac" → `/download/`
- Secondary action: "Read the docs" → `/docs/`
- Search phrases: private dictation for Mac; offline dictation app Mac; local speech to text Mac; AI dictation Apple Silicon; free dictation app macOS
- Objections: does my voice leave my Mac; will it change what I meant; is it better than built-in dictation; will it work in my apps and on my Mac; what does it cost
- Analytics and cookies: Amplitude loads on every page through `site/public/site-stats.js`, with autocapture and a browser cookie, plus the `download` and `docs_search` events. Session Replay is off. It doesn't load for visitors who turn it off on `/docs/privacy/` or whose browser sends Global Privacy Control. The site asks no consent up front. Disclosed on `/docs/privacy/` under "This website".

## Home sections

| Section | Why it stays |
| --- | --- |
| Hero, with the app screenshot | Says what it is, who it's for and how to get it, and shows the real app |
| How it works | Answers "is this hard to use?" |
| Examples | Answers "will it change what I meant?" and "is it better than built-in dictation?" |
| Privacy | Answers "does my voice leave my Mac?" |
| Features | Answers "why this and not a plain transcriber?" |
| FAQ | The remaining objections: Macs, apps, languages, cost |
| Final CTA | Asks again at the end of the page |

Cut in this round: the Native/Streaming/On-device stat row (repeated Privacy and How it works) and "Built in the open" (repeated the star, issue and open-source links found in the hero, privacy list, FAQ, final CTA and footer).

## Claims

| Claim on the page | Evidence |
| --- | --- |
| Recording, transcription, cleanup and learning run on the Mac; audio and text never leave it | `tauri/src-tauri/src/dictation/`, `backend/services/`, `site/src/content/docs/docs/privacy.md`. True of public v0.9.0; see the shared-Mac note below |
| Goes online for models and model details (Hugging Face), update checks (GitHub), practice sounds (openslr.org, zenodo.org) and daily usage counts | `backend/services/voice_training/sounds.py`, `backend/services/usage_report.py`, `docs/privacy` |
| Anonymous usage counts, never your words, and you can turn them off | `backend/services/usage_report.py`, on by default; no text, audio or IP set |
| This website's analytics: what Amplitude records and how to turn it off | `site/public/site-stats.js`, Amplitude Browser SDK 2.42.4 loaded from the project's CDN script, `site/astro.config.mjs` (`docs_search`), `site/src/pages/index.astro` (`download`). Opt-out and Global Privacy Control checked in headless Chrome, 2026-11-06, with Amplitude stubbed |
| Apple Silicon (M1 or later) | MLX is arm64-only; `.github/workflows/release.yml` builds arm64 only |
| macOS 15 or later | `libmlx.dylib` in Kass 0.10.0-beta.6 has `minos 15.0` (`otool -l`, 2026-11-06). The release builds on `macos-15`, as v0.9.0 did, so pip picks the macOS 15 MLX wheel. The app's own `minimumSystemVersion` of 11.0 is out of date |
| About 2.5 GB of disk space | `site/src/content/docs/docs/install.md`, default model sizes plus 500 MB headroom |
| Free, open source, MIT, no account | `LICENSE`, public repository, no sign-in code |
| Works in any app with a text field; restores the clipboard after pasting | `tauri/src-tauri/src/text_insert.rs`, `tauri/src-tauri/src/insert_chain.rs` |
| Style changes from "use formal mode" at the start of a dictation | `backend/services/styles.py`, honoured only at the start |
| Seven languages, plus auto-detect | `app/src/lib/constants/languages.ts` |
| Captures screenshot | Real app with example data, `docs/assets/readme/captures.png`, commit 87e9b738 (2026-09-30), version 0.6.1, said so in the caption |
| Dictation demo in the hero | Drawn illustration, labelled "Illustration" on the page |
| Share image | Crop of the same screenshot, `site/public/og-kass-v1.png` |

Shared-Mac note: the beta `network` feature can send takes to another Mac the user has set up (`tauri/src-tauri/src/dictation/server.rs`). It's opt-in and isn't in public v0.9.0, so "None of it leaves your Mac" holds today. Add the exception to the home page and `/docs/privacy/` before that feature reaches a public release (backlog B772).

## Waiting on a person

- B2 · Blocker, part · Retention isn't on the privacy page · Morgan: find how long Amplitude keeps the website's events (Amplitude settings; the API doesn't show it) and add the period to "This website" on `/docs/privacy/`
- B2 · Decision · The site sets Amplitude's cookie with no up-front consent · Morgan: decide whether EU and UK visitors need a consent prompt
- B3 · Major, part · The screenshot shows v0.6.1 · Morgan: retake `docs/assets/readme/captures.png` on the current version with example data, then redo the crop and bump `og-kass-v1.png`
- B5 · Minor, part · No Person or Organization JSON-LD · Morgan: say whether to be named as the maintainer
- B15 · Note · H1 has no search words · Morgan: decide whether "Say it messy. Send it clean." stays. It stays for now
