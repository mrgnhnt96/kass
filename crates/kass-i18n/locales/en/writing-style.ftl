### Kass UI strings, section "writingStyle" (converted from i18next).

writing-style-settings-title = Writing style
writing-style-settings-description = Teach Kass how you write so dictation comes out ready to send, in your own words. Everything stays on this device.
writing-style-settings-calibrate-title = Teach Kass how you write in { $style }
writing-style-settings-calibrate-description = Reply to a few messages the way you normally would, by voice or by typing. Kass learns from how you actually write.
writing-style-settings-calibrate-description-apps = Reply to a few messages like the ones you write in { $apps }, by voice or by typing. Kass learns from how you actually write.
writing-style-settings-calibrate-last-run =
    { $count ->
        [one] Last run { $date } · { $count } run so far
       *[other] Last run { $date } · { $count } runs so far
    }
writing-style-settings-calibrate-habits-label = Learned in { $style }
writing-style-settings-calibrate-start = Start
writing-style-settings-calibrate-again = Teach more
writing-style-settings-learns-from = What { $style } learns from
writing-style-settings-export-title = Export corrections
writing-style-settings-export-description = Save every correction you've made as a JSON file.
writing-style-settings-export-action = Export…
writing-style-settings-reset-title = Reset { $style }
writing-style-settings-reset-description = Forget what { $style } learned from your replies, the punctuation habits learned from them and from corrections, and its rules. Other styles keep theirs. Match my writing switches back to Standard.
writing-style-settings-reset-action = Reset…
writing-style-settings-reset-confirm-title = Reset { $style }?
writing-style-settings-reset-confirm = Reset
writing-style-settings-recent-title = Recent corrections in { $style }
writing-style-settings-recent-empty = No corrections yet. Use Teach Kass on a capture to fix what it got wrong.
writing-style-settings-recent-today = today
writing-style-settings-recent-yesterday = yesterday
writing-style-settings-recent-remove = Remove correction
writing-style-settings-examples-title = Your examples
writing-style-settings-examples-description =
    { $count ->
        [one] { $count } example of what you said and what you meant. Cleanup shows the most recent ones on every dictation.
       *[other] { $count } examples of what you said and what you meant. Cleanup shows the most recent ones on every dictation.
    }
writing-style-settings-examples-empty = No examples yet. Use Teach Kass on a capture, or teach this style above.
writing-style-settings-examples-view = View
writing-style-settings-examples-dialog-title = Your examples
writing-style-settings-examples-dialog-description = Cleanup shows the model your most recent examples. Older ones are summarized into rules. Removing an example stops Kass from using it.
writing-style-settings-examples-said = You said
writing-style-settings-examples-meant = You meant
writing-style-settings-examples-from-correction = From a correction
writing-style-settings-examples-from-calibration = From teaching
writing-style-settings-examples-remove = Remove
writing-style-settings-examples-remove-failed = Could not remove the example
writing-style-settings-notes-title = Rules from older examples
writing-style-settings-notes-description =
    { $count ->
        [one] { $count } rule summarized from examples too old to show the model, so they keep counting.
       *[other] { $count } rules summarized from examples too old to show the model, so they keep counting.
    }
writing-style-settings-notes-empty = None yet. Once enough of your examples are older than the most recent ones, Kass summarizes them into rules while it's idle.
writing-style-settings-notes-view = View
writing-style-settings-notes-dialog-title = Rules from older examples
writing-style-settings-notes-dialog-description = Cleanup follows these on every dictation. A new version is only kept when replaying your examples with it does no worse than the current rules.
writing-style-settings-notes-remove = Remove
writing-style-settings-notes-remove-failed = Could not remove the rule
writing-style-summary-title = Here's what Kass learned
writing-style-summary-habits-title = Punctuation habits
writing-style-summary-no-habits = No punctuation habits beyond Standard yet.
writing-style-summary-before = One of your dictations, cleaned up before
writing-style-summary-after = Cleaned up with what you just taught
writing-style-summary-use-learned = Also match my punctuation
writing-style-summary-keep-current = Keep current style
writing-style-summary-done = Done
writing-style-prompt-title = Make dictation write like you
writing-style-prompt-description = Reply to a few messages the way you'd really send them, so Kass learns how you write. About 3 minutes.
writing-style-prompt-start = Start
writing-style-prompt-dismiss = Not now
writing-style-learning-panel-title = Your writing style
writing-style-styles-title = Styles and their apps
writing-style-styles-description = Each style has its own punctuation and learns only from corrections made in its apps. Drag an app onto a style, or pick one in the list below. Click a style to filter the list and edit it.
writing-style-styles-column-label = { $style } apps
writing-style-styles-edit = Edit the { $style } style
writing-style-styles-count =
    { $count ->
        [one] { $count } app
       *[other] { $count } apps
    }
writing-style-styles-count-default =
    { $count ->
        [one] { $count } app · default
       *[other] { $count } apps · default
    }
writing-style-styles-move = Move { $app } to another style
writing-style-styles-move-to = Style for { $app }
writing-style-styles-new = New style
writing-style-styles-add = Add
writing-style-styles-new-hint = Starts with the default style's settings and nothing learned. Press Enter to add it.
writing-style-styles-default = Default
writing-style-styles-empty = Drop apps here
writing-style-styles-more = +{ $count } more
writing-style-styles-more-label =
    { $count ->
        [one] Show the { $count } app in { $style }
       *[other] Show all { $count } apps in { $style }
    }
writing-style-styles-apps-title = Apps you dictate into
writing-style-styles-apps-total =
    { $count ->
        [one] { $count } app, most dictated first.
       *[other] { $count } apps, most dictated first.
    }
writing-style-styles-new-notice =
    { $count ->
        [one] { $count } new app uses { $style } until you pick a style.
       *[other] { $count } new apps use { $style } until you pick a style.
    }
writing-style-styles-show-new = Show only new
writing-style-styles-keep-all = Keep all in { $style }
writing-style-styles-search = Search apps
writing-style-styles-filter-style = Style: { $style }
writing-style-styles-filter-new = New apps only
writing-style-styles-clear-filter = Clear filter
writing-style-styles-col-app = App
writing-style-styles-col-count = Dictations
writing-style-styles-col-style = Style
writing-style-styles-suggested = { $style } (suggested)
writing-style-styles-no-match = No apps match.
writing-style-styles-name-label = Style name
writing-style-styles-name-placeholder = Name
writing-style-styles-create-failed = Could not add the style
writing-style-styles-assign-failed = Could not change the app's style
writing-style-styles-memory = Each style keeps its own model cache of about { $mb } MB, which keeps switching apps fast. More styles use more memory.
writing-style-styles-limit =
    { $count ->
        [one] Kass keeps at most { $count } style, one model cache each.
       *[other] Kass keeps at most { $count } styles, one model cache each.
    }
writing-style-styles-move-dialog-title = Move { $app } to { $to }
writing-style-styles-move-dialog-description =
    { $count ->
        [one] { $app } has { $count } correction teaching { $from }. Bring it to { $to }, or leave it with { $from }? Either way, new corrections in { $app } teach { $to }.
       *[other] { $app } has { $count } corrections teaching { $from }. Bring them to { $to }, or leave them with { $from }? Either way, new corrections in { $app } teach { $to }.
    }
writing-style-styles-move-dialog-bring =
    { $count ->
        [one] Bring it to { $to }
       *[other] Bring { $count } to { $to }
    }
writing-style-styles-move-dialog-leave = Leave with { $from }
writing-style-styles-settings-title = { $style } settings
writing-style-styles-settings-name = Name
writing-style-styles-settings-punctuation-description = How thoughts join and how the text ends. Match my writing follows the habits this style learned from you.
writing-style-styles-settings-technical = Keep technical terms exact
writing-style-styles-settings-default = Default for new apps
writing-style-styles-settings-default-on = Apps you haven't put in a style use this one.
writing-style-styles-settings-default-description = Apps you haven't put in a style would use { $style }.
writing-style-styles-settings-make-default = Make default
writing-style-styles-settings-delete = Delete { $style }
writing-style-styles-settings-delete-description = Forgets what this style learned. Its apps move to the default style.
writing-style-styles-settings-delete-action = Delete…
writing-style-styles-settings-delete-confirm-title = Delete { $style }?
writing-style-styles-settings-save-failed = Could not save the style
writing-style-styles-settings-delete-failed = Could not delete the style
writing-style-styles-settings-description = How you write here
writing-style-styles-settings-description-help = In your own words: who you write to, how formal it is, and what's fine to skip. Kass follows it when it cleans up dictation in this style.
writing-style-styles-settings-description-placeholder = Mostly coworkers, so it's informal. Abbreviations like lmk are fine, run-on sentences are fine, no greetings or sign-offs…
writing-style-styles-settings-description-say = Say it
writing-style-styles-settings-description-stop = Stop
writing-style-habits-boundary-period = You end one thought and start the next as a new sentence
writing-style-habits-boundary-comma = You join related thoughts with commas
writing-style-habits-boundary-none = You run related thoughts together without punctuation
writing-style-habits-lowercase-start = You start sentences in lowercase
writing-style-habits-drop-intro-comma = You skip the comma after opening words like "yeah" or "so"
writing-style-habits-drop-conjunction-comma = You skip the comma before "and" or "but"
writing-style-habits-drop-final-period = You leave off the final period
writing-style-habit-chips-boundary-period = New sentence per thought
writing-style-habit-chips-boundary-comma = Commas join thoughts
writing-style-habit-chips-boundary-none = Thoughts run together
writing-style-habit-chips-lowercase-start = Lowercase starts
writing-style-habit-chips-drop-intro-comma = No comma after “so”
writing-style-habit-chips-drop-conjunction-comma = No comma before “and”
writing-style-habit-chips-drop-final-period = No final period
writing-style-teach-title = Teach Kass how you write
writing-style-teach-description = Reply to messages the way you normally would. Kass learns from what you send.
writing-style-teach-starting = Setting up your conversations…
writing-style-teach-failed = Something went wrong while teaching
writing-style-teach-any-order = { $style } · any order
writing-style-teach-progress = { $count } of { $target } replies
writing-style-teach-add-conversation = Add a conversation
writing-style-teach-finish = Finish
writing-style-teach-finish-note =
    { $count ->
        [one] Saves your reply as an example.
       *[other] Saves your { $count } replies as examples.
    }
writing-style-teach-finish-empty = Reply at least once to finish.
writing-style-teach-saved-on-close =
    { $count ->
        [one] Saved your reply
       *[other] Saved your { $count } replies
    }
writing-style-teach-subtitle = { $kind } · { $relation }
writing-style-teach-new-theme = New theme
writing-style-teach-wrap-up = Wrap up
writing-style-teach-wrapped = You wrapped up this conversation. Pick another one, or add a new one.
writing-style-teach-wrapped-short = Wrapped up
writing-style-teach-show-less = Show less
writing-style-teach-you = You
writing-style-teach-you-initial = Y
writing-style-teach-typing = { $persona } is typing…
writing-style-teach-for-you = For you
writing-style-teach-what-to-answer = What to answer
writing-style-teach-tell = Tell { $persona }:
writing-style-teach-try-this = Try this:
writing-style-teach-reply-label = Your reply
writing-style-teach-reply-placeholder = Dictate or type your reply, the way you'd really send it
writing-style-teach-send = Send reply
writing-style-teach-send-short = send
writing-style-teach-dictate = Dictate
writing-style-teach-stop = Stop
writing-style-teach-listening = Listening…
writing-style-teach-finish-button = Click Stop when you're done.
writing-style-teach-finish-hold = Let go of
writing-style-teach-finish-hands-free = or, hands-free, press again
writing-style-teach-esc-cancels = Esc throws the take away.
writing-style-teach-cleaning = Cleaning up what you said…
writing-style-teach-answers-after = { $persona } answers after you send.
writing-style-teach-kind-menu-note = Picking one starts a new conversation of that kind. This one stays in the list.
writing-style-teach-kinds-own = For this style's apps
writing-style-teach-kinds-other = Other
writing-style-teach-kinds-all = Kinds of conversation
writing-style-teach-kinds-coding-agent-label = Instructions to a coding agent
writing-style-teach-kinds-coding-agent-short = Coding agent
writing-style-teach-kinds-design-feedback-label = Feedback on a design or change
writing-style-teach-kinds-design-feedback-short = Feedback
writing-style-teach-kinds-notes-label = Notes to yourself
writing-style-teach-kinds-notes-short = Notes
writing-style-teach-kinds-writeup-label = Longer write-up (plan, doc)
writing-style-teach-kinds-writeup-short = Write-up
writing-style-teach-kinds-team-chat-label = Team chat
writing-style-teach-kinds-team-chat-short = Team chat
writing-style-teach-kinds-issue-comment-label = Issue or PR comment
writing-style-teach-kinds-issue-comment-short = Issue comment
writing-style-teach-kinds-email-label = Email
writing-style-teach-kinds-email-short = Email
writing-style-teach-kinds-text-message-label = Text message
writing-style-teach-kinds-text-message-short = Text
writing-style-teach-tricks-change-of-mind-text = change your mind partway through, the way you would out loud.
writing-style-teach-tricks-change-of-mind-example = For example, start with one answer, say “no wait”, then give the real one. Kass should write only the real one.
writing-style-teach-tricks-change-of-mind-example-with = For example, say { $example }. Kass should write only what you settled on.
writing-style-teach-tricks-long-text = take your time and explain, the way you'd talk it through.
writing-style-teach-tricks-long-example = It's fine to ramble; Kass learns how to put it in order.
writing-style-teach-tricks-long-example-with = It's fine to ramble, like { $example }.
writing-style-teach-chips-correction-resolved = change of mind cleaned up
writing-style-teach-chips-term = { $value } spelled right
writing-style-teach-chips-kept-exact = { $value } kept exact
writing-style-teach-chips-greeting = “{ $value }” greeting
writing-style-teach-chips-no-greeting = no greeting
writing-style-teach-chips-sign-off = “{ $value }” sign-off
writing-style-teach-chips-list = a list
writing-style-teach-chips-line-per-thought = one line per thought
writing-style-teach-chips-lowercase-start = lowercase starts
writing-style-teach-chips-no-final-period = no final period
writing-style-teach-chips-abbreviation = “{ $value }”
writing-style-teach-chips-emoji = { $value } emoji
writing-style-teach-chips-short = short
writing-style-teach-chips-run-on = run-on sentences
writing-style-teach-summary-saved =
    { $count ->
        [one] { $count } reply saved · { $dictated } dictated
       *[other] { $count } replies saved · { $dictated } dictated
    }
writing-style-teach-summary-examples = Your dictated replies are saved as examples, and Kass uses them on your next dictations.
writing-style-teach-summary-typed-only = You typed every reply, so Kass kept them but has no dictation to learn cleanup from. Dictate a few next time.
writing-style-teach-summary-keep-teaching = Keep teaching
