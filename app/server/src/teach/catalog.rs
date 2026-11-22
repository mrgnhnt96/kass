//! The kinds of conversation a teach session holds, and the hand-written
//! openers each starts from. Python: `backend/services/teach.py`.

/// A kind of conversation: who the user writes to, what they write, and
/// the apps where they write it.
pub struct Kind {
    pub id: &'static str,
    /// Who the user writes to, for the model.
    pub other_side: &'static str,
    /// What the user writes, for the model.
    pub medium: &'static str,
    pub bundles: &'static [&'static str],
    pub categories: &'static [&'static str],
}

const TERMINALS: [&str; 10] = [
    "com.mcclowes.saggar",
    "com.apple.Terminal",
    "com.googlecode.iterm2",
    "com.mitchellh.ghostty",
    "dev.warp.Warp-Stable",
    "net.kovidgoyal.kitty",
    "com.todesktop.230313mzl4w4u92",
    "com.microsoft.VSCode",
    "dev.zed.Zed",
    "com.exafunction.windsurf",
];

const TERMINALS_AND_FIGMA: [&str; 11] = [
    TERMINALS[0],
    TERMINALS[1],
    TERMINALS[2],
    TERMINALS[3],
    TERMINALS[4],
    TERMINALS[5],
    TERMINALS[6],
    TERMINALS[7],
    TERMINALS[8],
    TERMINALS[9],
    "com.figma.Desktop",
];

pub const KINDS: &[Kind] = &[
    Kind {
        id: "coding_agent",
        other_side: "an AI coding agent working in the user's software project",
        medium: "instructions typed to a coding agent in a terminal",
        bundles: &TERMINALS,
        categories: &["public.app-category.developer-tools"],
    },
    Kind {
        id: "design_feedback",
        other_side: "a designer or engineer who shared a design or a code change",
        medium: "feedback on a design or a code change",
        bundles: &TERMINALS_AND_FIGMA,
        categories: &["public.app-category.graphics-design"],
    },
    Kind {
        id: "notes",
        other_side: "the user's own notes app, prompting them to write something down",
        medium: "notes to themselves",
        bundles: &[
            "com.apple.Notes",
            "md.obsidian",
            "com.apple.reminders",
            "notion.id",
        ],
        categories: &[],
    },
    Kind {
        id: "writeup",
        other_side: "a teammate who asked for a written plan, notes or a doc section",
        medium: "a longer write-up such as a plan or a doc section",
        bundles: &[
            "com.apple.iWork.Pages",
            "com.microsoft.Word",
            "notion.id",
            "com.apple.TextEdit",
        ],
        categories: &["public.app-category.productivity"],
    },
    Kind {
        id: "team_chat",
        other_side: "a coworker in a team chat app",
        medium: "short team chat messages",
        bundles: &[
            "com.tinyspeck.slackmacgap",
            "com.microsoft.teams2",
            "com.hnc.Discord",
        ],
        categories: &["public.app-category.business"],
    },
    Kind {
        id: "issue_comment",
        other_side: "a coworker commenting on an issue or pull request",
        medium: "comments on an issue or pull request",
        bundles: &["com.linear", "com.github.GitHubClient"],
        categories: &[],
    },
    Kind {
        id: "email",
        other_side: "a colleague or someone the user deals with by email",
        medium: "emails",
        bundles: &[
            "com.apple.mail",
            "com.microsoft.Outlook",
            "com.superhuman.electron",
            "com.readdle.smartemail-Mac",
        ],
        categories: &[],
    },
    Kind {
        id: "text_message",
        other_side: "a friend or family member texting the user",
        medium: "text messages",
        bundles: &[
            "com.apple.MobileSMS",
            "net.whatsapp.WhatsApp",
            "org.whispersystems.signal-desktop",
            "ru.keepcoder.Telegram",
        ],
        categories: &["public.app-category.social-networking"],
    },
];

pub fn kind(id: &str) -> Option<&'static Kind> {
    KINDS.iter().find(|kind| kind.id == id)
}

/// A style with none of its own kinds starts with these.
pub const FALLBACK_KINDS: [&str; 3] = ["team_chat", "email", "text_message"];

/// Tricks that make a turn teach more than tone. The first turn of a
/// conversation is its seed's; later turns rotate through these.
pub const TRICKS: [&str; 6] = [
    "plain",
    "structure",
    "change_of_mind",
    "long",
    "tone",
    "terms",
];

/// What the model is told to do for a trick, when it's told anything.
pub fn trick_instruction(trick: &str) -> Option<&'static str> {
    match trick {
        "structure" => Some("Ask two or three separate things in this message."),
        "long" => Some(
            "Ask for something that needs a longer answer: a reason, a plan or an explanation.",
        ),
        "tone" => Some("Something has gone wrong or is urgent now; let it show in the message."),
        _ => None,
    }
}

/// The tricks the note names for the user: the rest only shape the message.
pub fn noted_trick(trick: &str) -> Option<&'static str> {
    match trick {
        "change_of_mind" => Some("change_of_mind"),
        "long" => Some("long"),
        _ => None,
    }
}

/// A hand-written opener: who writes, what they write, and the facts for the
/// answer.
pub struct Seed {
    pub id: &'static str,
    pub kind: &'static str,
    pub persona: &'static str,
    pub relation: &'static str,
    pub message: &'static str,
    /// One fact to pass on, or (question, answer) pairs.
    pub facts: Option<&'static str>,
    pub answers: &'static [(&'static str, &'static str)],
    pub title: Option<&'static str>,
    pub trick: &'static str,
    pub example: Option<&'static str>,
}

pub fn seeds_of(kind: &str) -> Vec<&'static Seed> {
    SEEDS.iter().filter(|seed| seed.kind == kind).collect()
}

/// The kinds that match `apps` ((bundle id, App Store category) pairs), in
/// catalog order: by bundle id first, then by category.
pub fn kinds_for_apps(apps: &[(String, Option<String>)]) -> Vec<String> {
    let has_bundle = |kind: &Kind| {
        apps.iter()
            .any(|(bundle, _)| kind.bundles.contains(&bundle.as_str()))
    };
    let has_category = |kind: &Kind| {
        apps.iter().any(|(_, category)| {
            category
                .as_deref()
                .is_some_and(|category| !category.is_empty() && kind.categories.contains(&category))
        })
    };
    let mut found: Vec<String> = KINDS
        .iter()
        .filter(|kind| has_bundle(kind))
        .map(|kind| kind.id.to_string())
        .collect();
    let by_category: Vec<String> = KINDS
        .iter()
        .filter(|kind| has_category(kind) && !found.iter().any(|id| id == kind.id))
        .map(|kind| kind.id.to_string())
        .collect();
    found.extend(by_category);
    found
}

pub const SEEDS: &[Seed] = &[
    Seed {
        id: "agent-retry",
        kind: "coding_agent",
        persona: "Coding agent",
        relation: "working in your project",
        message: "I added the retry to the upload request and the tests pass. Two questions before I go on:\n1. Should it retry on 500 errors too, or only on timeouts?\n2. Should the retry count be a setting, or hard-coded at 3?",
        facts: None,
        answers: &[
            ("500 errors?", "Only timeouts"),
            ("Retry count?", "Hard-code it at 3 for now"),
        ],
        title: None,
        trick: "change_of_mind",
        example: Some("“retry on 500s too… no wait, only on timeouts”"),
    },
    Seed {
        id: "agent-build",
        kind: "coding_agent",
        persona: "Coding agent",
        relation: "working in your project",
        message: "The build on main is failing after the dependency bump. I can pin the old version of the date library, or update our code to its new API. Which do you want?",
        facts: Some("Update the code to the new API, and add a test for date formatting."),
        answers: &[],
        title: None,
        trick: "long",
        example: None,
    },
    Seed {
        id: "agent-config",
        kind: "coding_agent",
        persona: "Coding agent",
        relation: "working in your project",
        message: "I found three places that parse the config file by hand. Want me to merge them into one helper? It touches the CLI, the server and the tests.",
        facts: Some("Yes, but do the CLI and the server first, and leave the tests for a second change."),
        answers: &[],
        title: None,
        trick: "plain",
        example: None,
    },
    Seed {
        id: "design-settings",
        kind: "design_feedback",
        persona: "Riley",
        relation: "a designer",
        message: "Here's the new settings page. I moved the account section to the top and made the save button sticky. Thoughts?",
        facts: Some("Account at the top is good. The sticky button covers the last row in small windows."),
        answers: &[],
        title: None,
        trick: "plain",
        example: None,
    },
    Seed {
        id: "design-pr",
        kind: "design_feedback",
        persona: "Sam",
        relation: "an engineer",
        message: "PR is up for the onboarding flow, screenshots are in the description. Anything you want changed before I merge?",
        facts: None,
        answers: &[
            ("Must fix?", "The skip button is too hard to find"),
            ("Nice to have?", "Bigger progress dots"),
        ],
        title: None,
        trick: "structure",
        example: None,
    },
    Seed {
        id: "design-icons",
        kind: "design_feedback",
        persona: "Riley",
        relation: "a designer",
        message: "Which icon set do you like for the toolbar, the outlined one or the filled one?",
        facts: Some("Outlined, but filled for the active state."),
        answers: &[],
        title: None,
        trick: "change_of_mind",
        example: Some("“the filled one… actually no, outlined, with filled for the active state”"),
    },
    Seed {
        id: "notes-meeting",
        kind: "notes",
        persona: "Notes",
        relation: "a note to yourself",
        message: "Your meeting with the design team just ended. What do you want to remember?",
        facts: Some("The header gets darker, mockups are due Friday, and ask Dana about the invite bug."),
        answers: &[],
        title: None,
        trick: "structure",
        example: None,
    },
    Seed {
        id: "notes-groceries",
        kind: "notes",
        persona: "Notes",
        relation: "a note to yourself",
        message: "Grocery run tonight. What do you need?",
        facts: Some("Eggs, coffee, dish soap, and something for dinner on Thursday."),
        answers: &[],
        title: None,
        trick: "plain",
        example: None,
    },
    Seed {
        id: "notes-idea",
        kind: "notes",
        persona: "Notes",
        relation: "a note to yourself",
        message: "You had an idea on the walk home. Get it down before you lose it.",
        facts: Some("Group captures by app, maybe with counts, and ask people whether they want search."),
        answers: &[],
        title: None,
        trick: "long",
        example: None,
    },
    Seed {
        id: "writeup-plan",
        kind: "writeup",
        persona: "Dana",
        relation: "your manager",
        message: "Can you write up a short plan for moving settings to the new layout? Just the steps and what could go wrong.",
        facts: Some("Move Account first, then Notifications, then Advanced. The risk is people not finding Advanced."),
        answers: &[],
        title: None,
        trick: "long",
        example: None,
    },
    Seed {
        id: "writeup-release",
        kind: "writeup",
        persona: "Dana",
        relation: "your manager",
        message: "Could you write this week's release notes? The main things are faster startup and the new dictionary page.",
        facts: Some("Startup is about twice as fast, the dictionary teaches words and replacements, and the crash on empty captures is fixed."),
        answers: &[],
        title: None,
        trick: "structure",
        example: None,
    },
    Seed {
        id: "writeup-readme",
        kind: "writeup",
        persona: "Kai",
        relation: "a teammate",
        message: "We need a paragraph for the README on setting up the dev environment. Can you write it?",
        facts: Some("Install bun and Python 3.12, run just setup, then just dev."),
        answers: &[],
        title: None,
        trick: "plain",
        example: None,
    },
    Seed {
        id: "chat-ship",
        kind: "team_chat",
        persona: "Dana",
        relation: "a coworker",
        message: "hey, are we still good to ship the onboarding changes Thursday? QA flagged something on the invite screen, not sure if it's a blocker",
        facts: Some("It's not a blocker. You'll have a fix up Wednesday, so Thursday still works."),
        answers: &[],
        title: Some("Slack DM"),
        trick: "plain",
        example: None,
    },
    Seed {
        id: "chat-review",
        kind: "team_chat",
        persona: "Kai",
        relation: "a coworker",
        message: "can you look at my PR when you get a sec? it's the settings page one",
        facts: Some("Yes, after lunch. Ask whether it needs the migration too."),
        answers: &[],
        title: Some("Slack DM"),
        trick: "plain",
        example: None,
    },
    Seed {
        id: "chat-status",
        kind: "team_chat",
        persona: "Alex",
        relation: "your lead",
        message: "Quick one before standup: where are we on the payments work?",
        facts: None,
        answers: &[
            ("Status?", "Blocked: the sandbox keys expired"),
            ("Plan?", "Finance is sending new keys; you're on the settings page meanwhile"),
        ],
        title: Some("Slack DM"),
        trick: "structure",
        example: None,
    },
    Seed {
        id: "issue-crash",
        kind: "issue_comment",
        persona: "Riley",
        relation: "a coworker",
        message: "Reproduced on staging: submitting the invite form with an empty email field crashes the screen. Can you take this before Thursday's release?",
        facts: Some("Yes. It's a missing check on the email field; you'll have a fix up Wednesday."),
        answers: &[],
        title: Some("Invite screen crashes on empty email"),
        trick: "plain",
        example: None,
    },
    Seed {
        id: "issue-search",
        kind: "issue_comment",
        persona: "Sam",
        relation: "a coworker",
        message: "Search takes 8 seconds on accounts with over 10k items. Any idea what changed?",
        facts: Some("The new ranking query skips the index. You'll add one and test again."),
        answers: &[],
        title: Some("Search is slow on large accounts"),
        trick: "long",
        example: None,
    },
    Seed {
        id: "issue-dark",
        kind: "issue_comment",
        persona: "Kai",
        relation: "a coworker",
        message: "Is anyone working on this? Happy to pick it up.",
        facts: Some("No one is. Go ahead, and reuse the color tokens from the settings page."),
        answers: &[],
        title: Some("Dark mode for the export page"),
        trick: "plain",
        example: None,
    },
    Seed {
        id: "email-budget",
        kind: "email",
        persona: "Priya Nair",
        relation: "a colleague",
        message: "Hi,\n\nCan we go over the Q3 budget before Tuesday's meeting? The travel line looks a lot higher than last quarter and I want to make sure we can explain it.\n\nDoes Monday afternoon work for you?\n\nPriya",
        facts: Some("Monday is full; Tuesday morning works. The conference was counted twice."),
        answers: &[],
        title: Some("Budget review before Tuesday?"),
        trick: "plain",
        example: None,
    },
    Seed {
        id: "email-intro",
        kind: "email",
        persona: "Jordan Lee",
        relation: "a colleague",
        message: "Hi,\n\nI'd like to introduce you to Alex, who's leading the redesign. I think you two should talk about the onboarding flow.\n\nJordan",
        facts: Some("Thank Jordan, say hi to Alex, and suggest a call next week."),
        answers: &[],
        title: Some("Intro: Alex from the design team"),
        trick: "plain",
        example: None,
    },
    Seed {
        id: "email-lease",
        kind: "email",
        persona: "Sam Ortiz",
        relation: "your landlord",
        message: "Hello,\n\nYour lease ends on the 31st. Would you like to renew for another year at the same rent? Please let me know by Friday.\n\nThanks,\nSam",
        facts: None,
        answers: &[
            ("Renew?", "Yes, for a year"),
            ("Anything else?", "Ask about fixing the dishwasher"),
        ],
        title: Some("Lease renewal"),
        trick: "structure",
        example: None,
    },
    Seed {
        id: "text-tonight",
        kind: "text_message",
        persona: "Sam",
        relation: "a friend",
        message: "are you still coming tonight?",
        facts: Some("Yes, but a little late; traffic is bad."),
        answers: &[],
        title: None,
        trick: "plain",
        example: None,
    },
    Seed {
        id: "text-photos",
        kind: "text_message",
        persona: "Mom",
        relation: "family",
        message: "did you get the photos I sent",
        facts: Some("Yes, the one with the dog is your favorite. Ask when she's visiting."),
        answers: &[],
        title: None,
        trick: "plain",
        example: None,
    },
    Seed {
        id: "text-lunch",
        kind: "text_message",
        persona: "Jess",
        relation: "a friend",
        message: "want to grab lunch tomorrow?",
        facts: Some("Tomorrow doesn't work; Thursday does. Suggest the taco place."),
        answers: &[],
        title: None,
        trick: "change_of_mind",
        example: Some("“tomorrow works… actually no, Thursday”"),
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    fn apps(list: &[(&str, Option<&str>)]) -> Vec<(String, Option<String>)> {
        list.iter()
            .map(|(bundle, category)| (bundle.to_string(), category.map(str::to_string)))
            .collect()
    }

    #[test]
    fn kinds_match_by_bundle_then_category() {
        assert_eq!(
            kinds_for_apps(&apps(&[
                ("com.tinyspeck.slackmacgap", None),
                (
                    "com.example.Editor",
                    Some("public.app-category.developer-tools")
                ),
            ])),
            ["team_chat", "coding_agent"]
        );
        assert_eq!(
            kinds_for_apps(&apps(&[("com.apple.Terminal", None)])),
            ["coding_agent", "design_feedback"]
        );
        assert!(kinds_for_apps(&apps(&[("com.example.Unknown", None)])).is_empty());
    }

    #[test]
    fn every_kind_has_seeds_and_every_seed_a_kind() {
        for kind in KINDS {
            assert!(seeds_of(kind.id).len() >= 3, "{}", kind.id);
        }
        for seed in SEEDS {
            assert!(super::kind(seed.kind).is_some(), "{}", seed.id);
            assert!(TRICKS.contains(&seed.trick), "{}", seed.id);
            assert!(
                seed.facts.is_some() != !seed.answers.is_empty(),
                "{}",
                seed.id
            );
        }
    }
}
