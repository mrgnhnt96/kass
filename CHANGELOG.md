# Changelog

Notable changes to Kass for users. Each release gets a section here, newest first. The website shows this file at [kass.mrgnhnt.com/changelog](https://kass.mrgnhnt.com/changelog/).

## 0.9.0 — October 9, 2026

### New

- **Fix what Kass just typed by voice.** Start a dictation with "fix that", "fix" or "edit" to change the text before your cursor: "fix that, Morgan not Megan", "edit, change Tuesday to Thursday", "fix that, delete actually", or spell a name out. No selecting needed. Turn it off with **Voice edits** in Settings › Dictation. Works where Kass can edit the field directly (not yet in Safari pages or Firefox).
- **A correction in Captures fixes the text where Kass typed it.** Save a correction and Kass also fixes the words in the app you dictated into, as long as they're still how Kass left them. In apps like Slack, the fix waits until you switch back.
- **Check for updates from Settings.** Click **Check for updates** beside the version to get the newest release now instead of waiting for the background check.

### Improved

- **Say a phrase again without doubling it.** Put the cursor mid-sentence and dictate the new words, running on into the words already after the cursor: `Let's meet | at noon tomorrow.` plus "on Friday at noon tomorrow" now gives `Let's meet on Friday at noon tomorrow.`
- **Dictionary names can leave similar names alone.** Turn off **Also fix words that sound like it** on an entry so a `Meghan` entry stops turning a real `Megan` into `Meghan`.
- **Undo takes back everything a correction taught.** Undo or Remove on a correction in Captures also drops the names, habits and rules Kass learned from it. Corrections, voice fixes included, teach Kass right away instead of hours later.

These were in beta and are now on for everyone.

## 0.8.0 — October 8, 2026

### New

- **Phrases.** Teach Kass a phrase like "insert my email" and saying it writes your text exactly, line breaks and all. Add one under **Dictionary › Phrases** by typing or saying it, or turn a correction into one with **Make a phrase**.
- **Reports.** When something goes wrong, the **Reports** tab makes a zip of Kass's logs, recent crash reports and system details to send us. Your captures, audio, dictionary and styles are never included, and your user and computer names are removed. If Kass won't start, run `kass-report.sh` from the app to make the same report.

### Improved

- **Add a corrected word to the dictionary in one click.** **Add to dictionary** beside a change adds the word and saves the correction, with no dialog.
- **Fast after time away.** Kass keeps its models ready while you're at your Mac, so the first dictation after a long break no longer waits for them to load back in.
- **Dictation doesn't wait behind a style.** Getting a style ready ahead of time no longer delays recognizing what you just said.
- **AirPods catch your first words.** Dictation with a Bluetooth headset starts on your Mac's built-in mic and moves to the headset once it's ready, so the start of what you say isn't lost.

### Fixed

- Long dictations no longer end in invented, sometimes foreign words.
- When the cleanup leaves out part of what you said, or swaps "you" and "I", Kass pastes what you said instead of the cleaned-up text.
- Saying the same words that end the text before the cursor ("Another one" after "Another one") no longer drops them.
- Anonymous usage stats are sent again.
- Error logs no longer include dictated text.

## 0.7.4 — October 6, 2026

### New

- **Write how it was said.** Say something with energy and Kass ends it with "!" when the words fit; draw a word out and it's written that way ("wayyy"). It adds no time after you release the keys. Remove or add one in your edits and Kass learns how you like it. Turn it off with **Write how it was said** in Settings › Transcription.
- **Cleaner text from your first take.** Kass now ships cleanup models it trained for each model size, so your text reads well before your Mac has trained on your own dictations, and your own training builds on them. Turn this off with **Use Kass's trained models** in Settings › Transcription.
- **Apps can get a text field ready for you to dictate into.** When you start dictating, Kass now tells the app in front first, so apps without a regular text field, like terminals, can show one for Kass to type into. Apps opt in, and nothing changes in apps that don't. App developers: see [Dictation handshake](https://github.com/mrgnhnt96/kass/blob/main/docs/DICTATION_HANDSHAKE.md).

### Improved

- **Kass spells its own name.** "Kass" is now in every dictionary to start, instead of "Cass" or "Kas". Edit or delete it like any other word; deleted, it stays deleted.

### Removed

- **Read Aloud.** Kass is for dictation, so the keys that read your selection aloud are gone, and so is their step in setup. If you downloaded its voice model (Kokoro, about 345 MB), you can delete the `models--mlx-community--Kokoro-82M-bf16` folder in `~/.cache/huggingface/hub` to get the space back.

## 0.7.3 — October 2, 2026

### New

- **Hear what you select.** Select text in any app and press <kbd>right ⌥</kbd> + <kbd>right ⇧</kbd>: Kass reads it to you in a natural voice, on your Mac. Press the keys again, press Escape, or click the pill to stop. Pick from 28 English voices and a speed in **Settings › Read Aloud**, which also downloads the voice model (Kokoro, about 345 MB) the first time. New setups offer it too, as an optional step you can skip.
- **Anonymous usage stats.** Once a day, Kass sends counts like how many words you dictated, time saved and how fast text appears, so we can see where it's slow. Never your words, audio or apps. It's on by default; turn it off at the end of setup or in **Settings › General**. See [Privacy](https://kass.mrgnhnt.com/docs/privacy/#usage-stats) for the full list.

## 0.7.2 — October 1, 2026

### New

- **Keep the text, not the recording.** Turn on **Delete voice recordings** in Settings › General to delete each recording as soon as its text is saved. Captures keep their text and say **Voice recording deleted automatically** where the player was. It applies to dictations from then on; earlier ones keep their audio until **Keep history** removes them. Corrections without a recording still teach your dictionary and rules, but no longer count toward model updates.
- **Say punctuation and Kass writes it.** "I'm home comma see you soon period" becomes `I'm home, see you soon.` Comma, period, full stop, question mark, exclamation point, colon and semicolon all work, and stay put through cleanup and your writing style. Talking about a mark still writes the word: "a comma", "a period of time". Corrections teach your own words for a mark, like "bang" for `!`.
- **Say how a word is capitalized.** "I love, in all caps" writes `I LOVE`, and "all caps yelling end caps" capitalizes the words in between. "Capital C-H-E-N-E-Y" spells `Cheney`.

### Improved

- **Fix any text by voice, not just Kass's (beta).** "Fix that" now works on the text before your cursor, whoever wrote it: words you typed, an older message, or a dictation Kass couldn't track. Only fixes to what Kass dictated teach it.
- **Fix with just the right word (beta).** "Fix, it's Thursday" changes the day, "it's 3:30" the time, and "fix that, Morgan" the name that sounds like it. "Fix" alone now starts a fix too.
- **Fix text in Messages (beta).** Where an app won't let Kass replace words directly, Kass selects them and types the fix.
- **Laughs are written as one word.** "Ha ha ha" is now `hahaha`, spelled the way you laughed, and a long laugh is no longer cut as a repeat.
- **Take back a voice fix (beta).** Deleting a "fix that" now undoes what it changed: its correction, what that taught, and any word it added to your dictionary (unless you've edited it since). Every correction in a capture's details now has **Remove**.

### Fixed

- **Every word you add to the dictionary goes into the correction.** Adding a second word from a saved correction used to only update the dictionary.
- **Deleting a capture keeps your place.** The selection moves to the next capture instead of jumping to the top.
- **Closing Kass's window leaves it ready.** Clicking Kass in the Dock after closing its window now brings the window back.
- **The permission prompt isn't hidden behind System Settings.** Kass now asks macOS first and opens Settings only once it's listed there.

## 0.7.1 — September 30, 2026

### New

- **Try new features early with beta updates.** Turn on **Beta updates** in Settings › General to get new features before they're public. Turn it off to go back to public releases; you stay on your beta until the next public one is newer.
- **Fix what Kass just typed by voice (beta).** Start a dictation with "fix that" to change your last one: "fix that, Morgan not Megan", "fix that, delete actually", or spell a name out. No selecting needed. Works where Kass can edit the field directly (not yet in Safari pages or Firefox).

### Improved

- **Dictionary names can leave similar names alone (beta).** With **Beta updates** on, you can stop a name from respelling similar ones. A `Meghan` entry used to also turn `Megan` and `Meagan` into `Meghan`. Turn off **Also fix words that sound like it** on an entry to fix only its exact spelling.
- **Say a phrase again without doubling it (beta).** With **Beta updates** on, put the cursor mid-sentence and dictate the new words, running on into the words already after the cursor: `Let's meet | at noon tomorrow.` plus "on Friday at noon tomorrow" now gives `Let's meet on Friday at noon tomorrow.` Kass drops the words you said again, even with a small spelling difference, and keeps the field's own. Live text may show them for a moment before it settles.

### Changed

- **Herga is now Kass.** Your captures, dictionary, writing styles and settings come along on their own. Because macOS treats a renamed app as a new one, Kass asks again for Microphone, Accessibility and Input Monitoring the first time it opens. The website moved to [kass.mrgnhnt.com](https://kass.mrgnhnt.com).

### Fixed

- **⌘H keeps Kass out of the way.** Dictating after hiding Kass with ⌘H used to bring its window back along with the pill. Now only the pill shows, and the window returns when you click Kass in the Dock or ⌘Tab to it.
- **The global keys work right after reinstalling Kass.** They used to wait until you brought Kass's window to the front. Now Kass notices on its own when macOS confirms the permissions you'd already given it.

## 0.6.2 — September 30, 2026

### Fixed

- **Writing styles learn when you leave off the final period.** Fixing a word in a capture used to count as keeping its period, so a style kept adding periods you'd removed. Now only the punctuation you actually change counts, and Herga recounts your past corrections the first time it opens.
- **Dictionary words are spelled right from the start.** A word in your dictionary used to be fixed only where Whisper already heard it right, so a misheard one stayed wrong until you'd corrected it. Now Herga also fixes near misses like `Kubernetis` or `cuber netes`, and leaves everyday words you actually said alone.
- **The global keys work right after Herga opens at login.** They used to wait until you'd opened Herga's window once.
- **Add a word to the dictionary while you correct it.** Select a word in a capture, even mid-correction, and just type its correct spelling. You no longer have to save the correction first.

## 0.6.1 — September 30, 2026

### Improved

- **Updates download in the background.** When a newer version is out, Herga downloads it while you work. Click **Restart** in the sidebar (or **Restart to update** in Settings › General) to switch to it, or it installs the next time you quit. Your permissions carry over. Versions before this one need a one-time manual update from the download page.

## 0.6.0 — September 30, 2026

The first release of Herga as a dictation app. This fork of [jamiepine/voicebox](https://github.com/jamiepine/voicebox) 0.5.0 drops text-to-speech and focuses entirely on turning your speech into ready-to-send text on Apple Silicon Macs.

**Voicebox is now Herga**, from *jerga*, Spanish for slang. Your captures, styles, dictionary and settings carry over the first time Herga opens. macOS treats it as a new app, so it asks for Microphone, Accessibility and Input Monitoring once more. The old Voicebox.app can go in the Trash; the install script does that for you.

### New

- **Guided setup.** First-run onboarding downloads the models in the background while it walks you through permissions and your hotkey, then has you say your name, read a messy line, reply to a text in your own words to teach your writing style, and rewrite a paragraph by voice. If macOS needs Herga to quit for a permission, setup reopens on the same step.
- **Native, streaming dictation.** The microphone is captured in native code and streamed while you speak, and cleanup runs a sentence at a time, so there's less to wait for when you let go.
- **Text lands where you were typing.** Text is inserted through Accessibility, typed keystrokes or the clipboard, whichever works for the app. When it has to use the clipboard, it puts yours back.
- **Writing styles per app.** Every app gets a style, and each style learns on its own. Switch style by saying so at the start of a dictation ("use formal mode"). The pill shows the style's name and plays a sound when it changes.
- **Teach by replying.** Teach a style how you write by replying to a few short conversations.
- **Dictionary.** Add terms and spoken replacements for everywhere, a style or specific apps. Select a word in a capture to add it.
- **Command Mode.** Select text in any app, hold <kbd>right ⌘</kbd> + <kbd>right ⇧</kbd> and say how to rewrite it. It comes with the Polish and Prompt Engineer transforms, and you can save your own.
- **Spoken commands.** Say line breaks, lists, quotes, brackets, braces, slashes, pipes and carets. Spell things out letter by letter ("capital C…"). Say "paste from clipboard" to insert your clipboard.
- **Self-corrections.** "Tuesday, no actually Wednesday" becomes "Wednesday". Repeats, stutters and restarts are removed.
- **Correction learning.** Fix a capture and the fix is used right away. Older fixes are folded into rules, which are kept only if they do at least as well on your past corrections.
- **Captures redesign.** Captures are grouped by app and show the audio next to what Whisper heard and the cleaned-up text. A **Check** badge flags cleanups worth a second look.
- **Insights.** See words dictated, speaking pace, time saved and your most-used apps.
- **History retention.** Choose how long to keep captures. Nothing is deleted until you confirm.
- **Escape to cancel** a dictation, even while it's still finishing.
- **Sound cues** for start, stop and errors, with a volume setting.
- **Launch at login**, on by default, with the window hidden.
- **Command palette.** Press <kbd>⌘</kbd> <kbd>K</kbd> to jump to any setting or action.
- **Update notices.** Herga tells you when a newer release is out.

### Improved

- After an update, Herga replaces a local server still running from the old version instead of reusing it, so the new version's backend is always the one running.
- Model weights stay loaded between dictations, and the first dictation after launch is as fast as the rest.
- The pill shows on the display you're working on, shows recording as soon as you press the keys, and never takes keyboard focus.
- Audio without a voice in it is never transcribed, and Whisper's repeated-phrase loops are removed.
- Cleanup is rejected and your own words are used instead when it answers your question instead of writing it down, copies an example, or adds words you didn't say.
- The microphone is released after every dictation.
- The DMG opens to a drag-to-Applications install window.

### Removed

- Text-to-speech, voice profiles and cloning, stories, effects, the MCP server, and Herga Cloud.
- Support for Windows, Linux and Intel Macs. Herga now runs only on Apple Silicon.
