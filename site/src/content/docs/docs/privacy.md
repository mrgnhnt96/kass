---
title: Privacy
description: What Kass stores, when it uses the internet, and what this website collects.
---

Kass is built so that your voice and your words stay on your Mac.

## What stays on your Mac

All of it:

- **Audio.** Your microphone is recorded by Kass's own native code and sent only to the local server on your Mac.
- **Transcription and cleanup.** Whisper and Qwen3 run on your Mac's GPU.
- **Your data.** Captures, corrections, your dictionary, writing styles, learned rules and stats live in a local SQLite database and folder. Open it from **Settings › General › Captures folder**.
- **Learning.** Correction rules and personal model training run locally. Training runs with Hugging Face access switched off.

The local server listens only on `127.0.0.1`, so other devices on your network can't reach it.

## When Kass uses the internet

- **Downloading models** from Hugging Face during setup, or when you pick a different model.
- **Model details.** The details pane in the Models tab loads a model's download count, likes and license from Hugging Face.
- **Voice training sounds.** If you train Kass on your voice, it downloads free speech and background-noise recordings from openslr.org and zenodo.org to practise against. Nothing of yours is sent.
- **Update check.** At launch and every six hours, Kass asks GitHub for the version number of the latest release so it can tell you when an update is out. Nothing about you or your dictations is sent.
- **Usage stats**, unless you turn them off. See below.

There's no account and no crash reporting. Once your models are downloaded, you can dictate with Wi-Fi off.

## Usage stats

To see how Kass is used and where it's slow, Kass sends anonymous usage counts to [Amplitude](https://amplitude.com) once a day. It's on unless you turn it off, at the end of setup or in **Settings › General › Share anonymous usage stats**.

For each day you dictated, it sends:

- how many dictations, words, seconds of speaking, time saved and fixes, and your speaking pace;
- how many takes went in and how many failed, and how long the text took to appear after you let go of the keys;
- Kass's version, your macOS version, your Mac's chip and memory;
- which speech and cleanup models you use, which features are on, and how many writing styles and dictionary words you have (as a range, like 11–50).

It never sends your words, your audio, your dictionary, your writing styles, or the apps you dictate into. The stats are sent under a random ID made for this, with no name, email or IP address attached. Only finished days are sent, and days you missed are caught up the next time Kass runs. Days when it's off are never sent, even if you turn it back on later.

## This website

The app's promises above are about the app. This website, kass.mrgnhnt.com, is separate and does collect analytics, also through [Amplitude](https://amplitude.com), in the United States. It's on unless you turn it off below.

When you visit, Amplitude records:

- the pages you view, your visits, and where you came from, including campaign tags in the link;
- what you click and interact with, such as links, buttons and forms;
- your browser, device and language, and your IP address, which Amplitude uses to work out a rough location;
- a random ID saved in a browser cookie, so that repeat visits count as the same visitor.

This site also sends two events of its own:

- `download`, when you click a Download button on the home page;
- `docs_search`, with the words you type into the docs search and how many results came back. Don't type anything private there.

It doesn't use Session Replay, so your screen isn't recorded, and it doesn't use advertising trackers. It never asks who you are, and nothing you dictate in the app ever reaches this website.

The site is hosted on GitHub Pages, and GitHub keeps its own server logs. Your browser also contacts GitHub directly to show the star count and to find the newest release on the download page. The [GitHub Privacy Statement](https://docs.github.com/en/site-policy/privacy-policies/github-general-privacy-statement) and [Amplitude's privacy notice](https://amplitude.com/privacy) cover what they do with that.

### Turn website analytics off

Your choice is saved in this browser only. If your browser sends Global Privacy Control, analytics stays off without you doing anything.

<p><button type="button" class="analytics-toggle" data-analytics-toggle hidden></button> <span data-analytics-status></span></p>
<noscript><p>Your browser isn't running JavaScript, so analytics isn't running either.</p></noscript>

## Deleting your data

- Delete individual captures from the **Captures** tab.
- Set **Keep history** to delete old captures automatically. See [Captures](/docs/captures/#keep-history).
- Turn on **Delete voice recordings** to keep only the text of each capture. See [Captures](/docs/captures/#delete-voice-recordings).
- To remove everything, quit Kass and delete its data folder, then delete your models from the Models tab or from the Hugging Face cache.

## Check for yourself

Kass is open source. Everything above can be checked in the [source code](https://github.com/mrgnhnt96/kass).
