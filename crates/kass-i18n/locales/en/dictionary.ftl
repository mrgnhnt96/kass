### Kass UI strings, section "dictionary" (converted from i18next).

dictionary-title = Dictionary
dictionary-description = Words dictation should get right, and phrases that write something for you. Pick where they apply on the left.
dictionary-scope-label = Where words apply
dictionary-scope-everywhere = Everywhere
dictionary-scope-styles = Writing styles
dictionary-scope-apps = Apps
dictionary-scope-count =
    { $count ->
        [one] { $count } word
       *[other] { $count } words
    }
dictionary-header-everywhere = Every dictation, in every app
dictionary-header-app-style = Uses the { $style } writing style
dictionary-header-style-apps =
    { $count ->
        [0] No apps use it yet
        [one] { $count } app uses it
       *[other] { $count } apps use it
    }
dictionary-kind-label = Words or phrases
dictionary-kind-words = Words
dictionary-kind-phrases = Phrases
dictionary-add-say = When I say
dictionary-add-write = Write
dictionary-add-say-placeholder = her ga
dictionary-add-write-placeholder = Kass
dictionary-add-action = Add
dictionary-add-hint = Leave “When I say” empty to only teach the spelling.
dictionary-add-limit = The dictionary is full at { $count } words. Delete some to add more.
dictionary-list-title = In { $scope } · { $count }
dictionary-list-order = Newest first
dictionary-list-spelling-only = spelling only
dictionary-list-empty = A replacement writes one thing when you say another. A spelling-only word, with “When I say” left empty, is always spelled as written.
dictionary-list-edit = Edit { $written }
dictionary-list-delete = Delete { $written }
dictionary-list-delete-failed = Couldn't delete that word
dictionary-list-confirm-delete = Remove everywhere it applies?
dictionary-list-remove = Remove
dictionary-list-cancel = Cancel
dictionary-list-save = Save
dictionary-list-applies-in = Applies in
dictionary-list-places =
    { $count ->
        [one] { $count } place
       *[other] { $count } places
    }
dictionary-list-no-places = No places
dictionary-list-app-uses-style = { $app } uses it
dictionary-list-everywhere-clears = Checking Everywhere clears the rest.
dictionary-list-just-now = just now
dictionary-list-minutes-ago = { $count }m ago
dictionary-list-hours-ago = { $count }h ago
dictionary-list-match-sound = Also fix words that sound like it
dictionary-list-match-sound-hint = Off: only this exact spelling is fixed, so a similar name (Megan for Meghan) is left alone.
dictionary-list-note-spoken-fix = spelled aloud
dictionary-list-note-spoken-fix-hint = Added when you spelled it aloud to fix it. Only this exact spelling is fixed; words that sound like it are left alone.
dictionary-list-note-correction = from a correction
dictionary-list-note-correction-hint = Added when you corrected it in a transcript, so it isn't misheard again. Withdrawing that correction removes it.
dictionary-list-note-exact = exact spelling
dictionary-list-note-exact-hint = Only this exact spelling is fixed; words that sound like it are left alone.
dictionary-inherited-title = Also applies in { $scope }
dictionary-inherited-from = From { $source }
dictionary-inherited-dropped =
    { $count ->
        [one] { $count } word doesn't fit in the speech model's prompt; it's still fixed after cleanup.
       *[other] { $count } words don't fit in the speech model's prompt; they're still fixed after cleanup.
    }
dictionary-phrases-write = Write exactly
dictionary-phrases-say-placeholder = insert my email
dictionary-phrases-write-placeholder = you@example.com
dictionary-phrases-dictate = Say the phrase
dictionary-phrases-stop = Stop
dictionary-phrases-listening = listening
dictionary-phrases-hint = Type the phrase, click the mic, or hold <keys/> to say it. The text goes in exactly as written.
dictionary-phrases-hint-another = Click the mic or hold <keys/> to say it another way.
dictionary-phrases-hint-release = Let go of <keys/> to fill in the phrase.
dictionary-phrases-hint-typed = Type what you say. The text goes in exactly as written.
dictionary-phrases-list-title = Phrases in { $scope } · { $count }
dictionary-phrases-empty = A phrase writes text for you when you say it: an email address, a link, a sign-off. Line breaks are kept.
