### Kass UI strings, section "captures" (converted from i18next).

captures-dictionary-add = Add to dictionary
captures-dictionary-title = Add to dictionary
captures-dictionary-description = Kass spells it this way from now on.
captures-dictionary-spelling = Correct spelling
captures-dictionary-submit = Add
captures-dictionary-added = Added “{ $written }” to the dictionary
captures-dictionary-add-failed = Could not add it to the dictionary
captures-phrase-make = Make a phrase
captures-phrase-title = Make a phrase
captures-phrase-description = Kass writes this text whenever you say the phrase.
captures-phrase-same-text = Same text as your “{ $spoken }” phrase.
captures-phrase-saves-correction = Your correction saves too.
captures-phrase-submit = Add phrase
captures-phrase-added = Added the phrase “{ $spoken }”
captures-phrase-chip = phrase
captures-phrase-from = from Dictionary
captures-phrase-wrote-hint = Written by your “{ $spoken }” phrase
captures-phrase-edit = Edit phrase
captures-feedback-report = Teach Kass
captures-feedback-description = Edit this into what you meant: fix wrong words, drop false starts, put things in order, fix grammar. Keep your own words and every idea you said. Kass uses it as an example on your next dictations right away, and the personal model trains on it while the app is idle. Everything stays on this device. Deleting a capture removes its corrections from future training.
captures-feedback-original = What Kass wrote ({ $target })
captures-feedback-expected = What you meant
captures-feedback-empty-hint = Leave empty if the correct output should contain no text.
captures-feedback-notes = What went wrong? (optional)
captures-feedback-save = Save
captures-feedback-saved = Saved. Kass will use this on your next dictations.
captures-feedback-failed = Could not save correction
captures-feedback-about = About corrections
captures-feedback-export-failed = Could not export corrections
captures-feedback-load-failed = Could not load saved corrections
captures-feedback-learning-title = Learning from corrections
captures-feedback-learning-model-title = Personal model improvement
captures-feedback-learning-dataset = Training: { $train } / 12 · Validation: { $validation } / 3 · Test recordings: { $test } / 5
captures-feedback-learning-personal-model = A validated personal adapter is active.
captures-feedback-learning-base-model = Using the base refinement model.
captures-feedback-learning-speech-model = Validated speech model: { $model }
captures-feedback-learning-evaluation = Held-out token errors: { $before } → { $after }
captures-feedback-learning-cancel-training = Cancel training
captures-feedback-learning-rollback-model = Undo model update
captures-feedback-learning-model-phases-waiting = Waiting for the next idle period.
captures-feedback-learning-model-phases-preparing = Preparing the training and evaluation datasets.
captures-feedback-learning-model-phases-waiting-data = More distinct corrected recordings are needed before model training.
captures-feedback-learning-model-phases-waiting-idle = Waiting until recording and model work finish.
captures-feedback-learning-model-phases-waiting-memory = Waiting for enough free memory.
captures-feedback-learning-model-phases-waiting-model = Download the selected refinement model before training can begin.
captures-feedback-learning-model-phases-training = Training candidate model weights in the background.
captures-feedback-learning-model-phases-evaluating = Comparing models on recorded audio.
captures-feedback-learning-model-phases-activated = A validated model improvement was activated.
captures-feedback-learning-model-phases-rejected = The candidate did not pass every quality and performance check. The current model was kept.
captures-feedback-learning-model-phases-paused = Background work paused for recording or other model work.
captures-feedback-learning-model-phases-interrupted = The previous job was interrupted. It will retry while idle.
captures-feedback-learning-model-phases-failed = The improvement job failed. The production model was kept.
captures-feedback-learning-model-phases-no-new-data = No new eligible data since the previous attempt.
captures-feedback-learning-model-phases-rolled-back = The previous model version was restored.
captures-feedback-learning-model-phases-reverted = The personal adapter failed and was disabled.
captures-feedback-learning-model-phases-unsupported = Local adapter training currently requires an Apple Silicon Mac.
captures-feedback-learning-description = The background job trains a personal refinement model and tests it on separate corrections and recorded audio. It also compares installed speech models. Only improvements that pass accuracy, speed and memory checks are activated. Recording takes priority, and updates can be undone.
captures-feedback-learning-active = Active corrections: { $count }
captures-feedback-learning-waiting = Waiting for the first background check.
captures-feedback-learning-updated = Validated corrections were updated.
captures-feedback-learning-no-change = No new corrections passed the checks. More distinct examples may be needed.
captures-feedback-learning-rolled-back = The previous correction version was restored. Withdrawn corrections will not be added again automatically.
captures-feedback-learning-last-run = Last checked: { $date }
captures-feedback-learning-too-slow = The candidate exceeded the speed limit and was not activated.
captures-feedback-learning-failed = Could not check or update learned corrections. Try again.
captures-feedback-learning-run = Check now
captures-feedback-learning-running = Working…
captures-feedback-learning-rollback = Undo last update
captures-title = Captures
captures-search-placeholder = Search captures
captures-apps-title = Apps
captures-apps-all = All apps
captures-apps-unknown = Unknown app
captures-apps-collapse = Collapse app list
captures-apps-expand = Expand app list
captures-apps-list-label = { $app } captures
captures-apps-new = New
captures-apps-no-style = No style yet
captures-apps-style-for = Style for { $app }
captures-apps-suggested = { $style }?
captures-apps-prompt = New app. Use { $style } for { $app }?
captures-apps-use = Use { $style }
captures-apps-pick-another = Pick another
captures-apps-dictations-teach =
    { $count ->
        [one] { $formatted } dictation · corrections teach { $style }
       *[other] { $formatted } dictations · corrections teach { $style }
    }
captures-apps-search-placeholder = Search { $app } captures
captures-apps-dictations =
    { $count ->
        [one] { $formatted } dictation
       *[other] { $formatted } dictations
    }
captures-snippet-empty = (no transcript)
captures-capture-card-label = Capture · { $when }
captures-tag-review = Reworded
captures-tag-raw = Raw
captures-tag-command = Command
captures-tag-command-failed = Not rewritten
captures-list-yesterday = yest
captures-not-ready-title = Dictation isn't set up yet
captures-not-ready-description = Finish setup to download the models and grant the permissions the shortcut needs.
captures-not-ready-action = Finish setup
captures-panel-copy = Copy text
captures-panel-copy-raw = Copy raw transcript
captures-teach-edit-hint = Click to correct this. Kass learns from your fix.
captures-teach-save = Save correction
captures-teach-learned = Learned
captures-teach-more-changes =
    { $count ->
        [one] +{ $count } more change
       *[other] +{ $count } more changes
    }
captures-teach-examples-learned =
    { $count ->
        [one] { $count } example learned
       *[other] { $count } examples learned
    }
captures-teach-undo = Undo
captures-teach-remove = Remove
captures-teach-undo-failed = Could not undo the correction
captures-teach-alter = Alter
captures-teach-corrected-by-you = Corrected by you
captures-teach-updated-in = Updated in { $app }
captures-teach-updated-in-app = Updated where you dictated it
captures-transcript-refined = Refined
captures-transcript-raw = Raw
captures-transcript-heard = Heard
captures-actions-configure = Style settings
captures-actions-configure-command = Command Mode settings
captures-actions-delete = Delete
captures-details-show = Show details
captures-details-hide = Hide details
captures-empty-no-matches = No captures match "{ $query }"
captures-empty-none-in-filter = No captures in this filter.
captures-empty-none = No captures yet.
captures-empty-loading = Loading captures…
captures-empty-pick-one = Pick a capture to see the transcript.
captures-empty-hold-to-record = Hold to record
captures-empty-toggle-hands-free = Toggle hands-free
captures-empty-press-shortcut = Press the shortcut anywhere on your machine to start your first capture.
captures-empty-turn-on-shortcut = Turn on the global shortcut to dictate from anywhere — or click Dictate above for an in-app capture.
captures-empty-open-settings = Open dictation settings
captures-delete-dialog-title = Delete capture
captures-delete-dialog-description = This will permanently delete the capture, its audio, and its transcript. This cannot be undone.
captures-delete-dialog-voice-edit-description = This will permanently delete the voice edit and its audio, and take back what it taught Kass: the correction it saved on the take it fixed, and any word it spelled into the Dictionary. The text it changed stays as it is.
captures-delete-dialog-deleting = Deleting…
captures-toast-delete-failed = Delete failed
captures-toast-transcript-copied = Transcript copied
captures-toast-copy-failed = Copy failed
captures-toast-export-success = Exported to { $path }
captures-toast-export-failed = Export failed
captures-toast-export-empty = Nothing to export
captures-toast-shortcut-not-armed = Shortcut on, but not yet armed
captures-toast-shortcut-not-armed-description =
    { $count ->
        [one] { $names } still needs to download. Open the Captures tab to start.
       *[other] { $names } still need to download. Open the Captures tab to start.
    }
captures-pill-preparing = Opening microphone…
captures-pill-recording = Recording
captures-pill-transcribing = Transcribing
captures-pill-refining = Refining
captures-pill-completed = Done
captures-pill-stop-aria = Stop recording
captures-pill-error-fallback = Something went wrong
captures-pill-error-copy-tooltip = Click to copy error
captures-chord-capturing = Capturing…
captures-chord-press-shortcut = Press your shortcut
captures-chord-no-keys = No keys yet
captures-chord-unsupported = "{ $key }" isn't supported in chords. Try a modifier or letter key.
captures-chord-not-set = Not set
captures-chord-captured = captured
captures-chord-save = Save shortcut
captures-readiness-title = A few things before you can dictate
captures-readiness-subheading = The shortcut stays off until everything below is ready.
captures-readiness-download-button = Download
captures-readiness-downloading = Downloading…
captures-readiness-downloading-percent = Downloading… { $pct }%
captures-readiness-download-started = Download started
captures-readiness-download-started-description = { $name } is downloading. The shortcut will arm itself when it finishes.
captures-readiness-download-failed = Download failed
captures-readiness-stt-label = { $name } (speech-to-text)
captures-readiness-stt-ready = Model downloaded.
captures-readiness-stt-missing = Needed to transcribe your audio
captures-readiness-stt-missing-with-size = Needed to transcribe your audio · { $size }
captures-readiness-llm-label = { $name } (refinement)
captures-readiness-llm-ready = Model downloaded.
captures-readiness-llm-missing = Cleans up the raw transcript before paste
captures-readiness-llm-missing-with-size = Cleans up the raw transcript before paste · { $size }
captures-readiness-input-monitoring-label = Input Monitoring permission
captures-readiness-input-monitoring-ready = macOS allows Kass to detect your global shortcut.
captures-readiness-input-monitoring-missing = macOS needs to allow Kass to detect the global shortcut.
captures-readiness-input-monitoring-open-settings = Open Settings
captures-readiness-accessibility-label = Accessibility permission
captures-readiness-accessibility-ready = Kass can paste transcriptions into other apps.
captures-readiness-accessibility-missing = Required so transcriptions can paste into the focused app.
captures-readiness-accessibility-open-settings = Open Settings
captures-permissions-accessibility-title = Grant Accessibility permission to enable auto-paste
captures-permissions-accessibility-body = Kass needs <path>System Settings → Privacy & Security → Accessibility</path> to paste transcriptions into other apps. Your dictation still lands in the Captures tab without it.
captures-permissions-accessibility-open-settings = Open Settings
captures-permissions-accessibility-recheck = I've enabled it
captures-permissions-accessibility-rechecking = Checking…
captures-permissions-accessibility-still-missing = Still not detected. macOS usually requires quitting and reopening Kass after toggling the permission.
captures-permissions-input-monitoring-title = Grant Input Monitoring to enable the global shortcut
captures-permissions-input-monitoring-body = Kass needs <path>System Settings → Privacy & Security → Input Monitoring</path> to detect your dictation chord. The toggle is on, but macOS is blocking key events until you allow it.
captures-permissions-input-monitoring-open-settings = Open Settings
captures-permissions-input-monitoring-recheck = I've enabled it
captures-permissions-input-monitoring-rechecking = Checking…
captures-permissions-input-monitoring-still-missing = Still not detected. macOS usually requires quitting and reopening Kass after toggling the permission.
captures-review-badge = Check
captures-review-title = Check this cleanup
captures-review-rejected-title = Your words were kept instead of the cleanup
captures-review-reasons-answered = The cleanup looked like a reply to what you said, not a cleanup of it.
captures-review-reasons-dropped = The cleanup left out part of what you said.
captures-review-reasons-negation = The cleanup changed a "not" or "never", which can flip the meaning.
captures-review-reasons-name = The cleanup left out a name you said.
captures-review-reasons-number = The cleanup changed a number.
captures-review-reasons-person = The cleanup swapped who does what, like "you" and "me".
captures-review-reasons-technical = The cleanup changed a technical term, like a file or variable name.
captures-review-added = May have added: { $words }
captures-review-missing = May have left out: { $words }
captures-review-hint = If it's wrong, type what you meant under Teach so Kass learns it.
captures-source-dictation = Dictation
captures-source-recording = Recording
captures-source-file = Imported file
captures-source-command = Command
captures-detail-today = Today
captures-detail-yesterday = Yesterday
captures-detail-audio-deleted = Voice recording deleted automatically
captures-changes-title = What changed
captures-changes-none = No changes
captures-changes-phrase =
    { $count ->
        [one] { $count } phrase
       *[other] { $count } phrases
    }
captures-changes-removed =
    { $count ->
        [one] −{ $count } word
       *[other] −{ $count } words
    }
captures-changes-added =
    { $count ->
        [one] +{ $count } word
       *[other] +{ $count } words
    }
captures-changes-reworded = { $count } reworded
captures-changes-case = { $count } capitalization
captures-changes-punctuation = { $count } punctuation
captures-inspector-recording = Recording
captures-inspector-length = Length
captures-inspector-pace = Pace
captures-inspector-wpm = { $count } wpm
captures-inspector-words = Words
captures-inspector-language = Language
captures-inspector-mac = Mac
captures-inspector-this-mac = This Mac
captures-inspector-other-mac = Another Mac
captures-inspector-models = Models
captures-inspector-speech = Speech
captures-inspector-refinement = Refinement
captures-inspector-whisper = Whisper { $model }
captures-inspector-qwen = Qwen3 { $model }
captures-inspector-corrections = Corrections
captures-inspector-corrections-count = Corrections · { $count }
captures-inspector-no-corrections = None yet. Alter the text to teach Kass.
captures-inspector-showing = Showing
captures-inspector-by-voice-edit = From a voice edit
captures-command-result = Rewrite
captures-command-original = Original selection
captures-command-instruction = Instruction
captures-command-said = Said
captures-command-transform = Transform
captures-command-copy-result = Copy rewrite
captures-command-copy-original = Copy original
captures-command-copied = Copied
captures-command-failed = This command didn’t produce a rewrite, so the selection was left as it was.
captures-command-undo-hint = To undo a rewrite, press ⌘Z in the app, or copy the original back in.
captures-command-rewriting = Rewriting
captures-week-label = Last 7 days
captures-week-label-app = Last 7 days · { $app }
captures-week-words-label = words
captures-week-wpm = wpm
captures-week-saved = saved
captures-week-open = Open Insights
captures-week-chart = Words per day in { $app }, last 7 days
captures-week-words =
    { $count ->
        [one] { $formatted } word
       *[other] { $formatted } words
    }
captures-week-no-dictation = No dictation
captures-week-today = today
