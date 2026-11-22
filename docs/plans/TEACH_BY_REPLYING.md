# Teach by Replying

## Problem

"Teach Kass how you write" showed five spoken paragraphs already cleaned up by Kass and asked the user to fix them. About 80% of each paragraph was given, so the edits showed little of how the user actually writes. The redesign (canvas "Teach by Replying", merged board) has the user reply to messages instead: every word of a reply is theirs.

## The flow

The dialog teaches one writing style. It has a sidebar of conversations and the open conversation on the right.

- **Conversations are ongoing.** Each starts from a hand-written opener. After each reply, the local cleanup model writes the other person's next message and the facts for the next reply. The user can reply as long as they like, open another conversation, or **Wrap up** one. Progress reads "N of 10 replies"; ten is a soft target, and Finish works after one reply.
- **Kinds.** A conversation is one of eight kinds: instructions to a coding agent, feedback on a design or change, notes to yourself, a longer write-up, team chat, an issue or PR comment, an email, a text message. The kinds that match the style's apps come first (by bundle id, then App Store category). A style starts with up to three conversations of its own kinds; with no apps it gets team chat, email and a text.
- **Header controls.** A kind picker (the same grouped list as "Add a conversation") starts a new conversation of the picked kind and keeps the current one in the list. **New theme** replaces the open conversation with a fresh one of the same kind. **Wrap up** ends it.
- **For you.** Above the reply box, a note gives the facts to pass on ("Tell Dana: …") or pairs each question with its answer ("Old paragraphs? → Keep them for now"), so the user only chooses the words. Some turns add **Try this** with an example, for a teaching trick (below).
- **Reply box.** The user dictates with their usual shortcut, or the Dictate button, or types. The hint follows how the take started: the button shows Stop; a shortcut take says how to finish either way. Esc throws a take away, as everywhere. There is no "Heard" line.
- **Picked up.** Each reply gets chips for what it showed (lowercase starts, no final period, a greeting, one line per answer, a term kept exact, a retraction cleaned up). A conversation shows one row of its chips, then "+N", which expands and scrolls.

### Teaching tricks

Turns rotate through tricks, so a run covers more than tone:

- **Change of mind:** "Try this: change your mind partway through", with an example. The saved example then teaches how the user wants spoken corrections resolved.
- **Structure:** the other side asks two or three things at once, so the reply needs lines or a list.
- **Long answer:** a question that needs 30+ seconds of talking, where cleanup has to reorder.
- **Tone shift:** the other side turns urgent or touchy partway through.
- **Your own terms:** the other side uses the user's dictionary terms, which teaches their spelling too.

## Where a reply comes from

Dictating into Kass's own window already works: a shortcut take types into the focused field, and a take from the Dictate button lands in Captures. While a teach session is open, dictation in Kass's own window is cleaned up in the style being taught (`styles.teaching`), so the shown cleanup is the one the user corrects.

When the user sends a reply, the server takes the captures made in Kass's window since that turn was shown: their raw transcripts are what was said and their cleanups what was shown. For a Dictate-button take the dialog asks for the same captures' cleanup and puts it in the box. A typed reply has no captures; it still gets chips, but teaches no cleanup.

## What is learned

Finishing saves each dictated reply the way calibration saved a rewrite: `said`, `shown` and `written` in the style's profile (`writing-style.json`). So everything that read calibration keeps working unchanged: examples in the cleanup prompt, punctuation habits (`observe(shown, written)`), Match my writing, reset. Typed replies are saved with `shown` equal to `written`, which makes them no example and no habit evidence.

Habits per kind ("in Slack you never greet") are counted for the chips only. Putting them in the cleanup prompt needs to know the kind at dictation time; that is later work.

## Generating the other side

The server's teach sessions (`app/server/src/teach`) ask the cleanup model (the user's size, no thinking) to continue the conversation in character. The prompt names the persona, the kind and this turn's trick, and asks for exactly two labelled lines, `MESSAGE:` and `TELL:`. `TELL` is either one fact or `question -> answer` pairs separated by `|`. A reply that can't be parsed falls back to the next hand-written opener of the same kind, as a new topic in the same conversation. Generation runs under its own prompt cache key (`teach`), so it never evicts a style's cleanup cache. It takes 1–3 s on 4B, shown as "… is typing".

## Writing style page

- **How you write here:** a description per style (`writing_styles.description`, at most 600 characters), with a Say it button. It goes into that style's cleanup prompt as the speaker's own description, with the rule that it never adds words or ideas.
- The "Capitalize the first word" and "Remove filler words" toggles are removed. The base prompt already removes fillers, and learned punctuation covers lowercase starts. Both flags stay on for every style; the columns stay, unused, so old captures' flags still replay.

## Not in this change

- Detecting who a message is to (a Slack DM's name from the window title) is later work, so the description can't hold per-recipient rules yet.
- Per-kind habits in the cleanup prompt.
- The paragraph calibration and its paragraphs are removed.

## Tests

- `app/server/src/teach` unit tests and `tests/teach.rs`: kinds for a style's apps; parsing `MESSAGE`/`TELL` and the fallback; replies linked to Kass-window captures since the turn; typed replies; reply chips; finish saving examples that `calibration_examples` and habits read; the teaching style override and its expiry.
- `test_writing_style.py`: the description in the prompt; flags always on.
- `app/tests`: chip overflow and kind grouping helpers.
