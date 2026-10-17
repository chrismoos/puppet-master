//! Whether an agent's answer can be read aloud as written.
//!
//! Replies are spoken exactly as the agent wrote them, so nothing here
//! rewrites one: it only says why a reply cannot be spoken, which is
//! handed back to the agent so it can answer again in words somebody
//! can listen to.

/// More words than this is not an answer somebody listens to, it is a
/// document. An agent is asked for thirty, so this leaves room for an
/// answer that ran a little over and catches one that ignored the
/// budget. Measured in words rather than characters because that is
/// what decides how long it takes to say.
const SPOKEN_WORDS_MAX: usize = 40;

/// A backstop for an answer whose words are not words. A wall of
/// identifiers counts as few of them and still cannot be listened to.
const SPOKEN_CHARS_MAX: usize = 320;

/// More lines than this is a list however it is punctuated.
const LINES_MAX: usize = 2;

/// Why this cannot be read aloud, in words that tell the agent what to
/// change. `None` means speak it as it stands.
pub fn trouble(body: &str) -> Option<String> {
    let text = body.trim();
    if text.contains("```") {
        return Some("it had a code block in it".into());
    }
    if text.contains("http://") || text.contains("https://") {
        return Some("it had a URL in it".into());
    }
    if let Some(path) = text.split_whitespace().find(|word| is_path(word)) {
        return Some(format!("it had a file path in it, {path}"));
    }
    let lines = text.lines().filter(|line| !line.trim().is_empty()).count();
    if lines > LINES_MAX {
        return Some(format!("it was {lines} lines long"));
    }
    let words = text.split_whitespace().count();
    if words > SPOKEN_WORDS_MAX {
        return Some(format!(
            "it was {words} words, and anything over {SPOKEN_WORDS_MAX} is too long to hear"
        ));
    }
    let characters = text.chars().count();
    if characters > SPOKEN_CHARS_MAX {
        return Some(format!(
            "it was {characters} characters, and anything over {SPOKEN_CHARS_MAX} is too long \
             to hear"
        ));
    }
    None
}

/// A path rather than prose. Two separators is the test that keeps
/// "and/or" and a bare "24/7" out of it.
fn is_path(word: &str) -> bool {
    let trimmed = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '/' && c != '.');
    trimmed.matches('/').count() >= 2
        || (trimmed.contains('/') && trimmed.rsplit('.').next().is_some_and(is_extension))
}

fn is_extension(suffix: &str) -> bool {
    !suffix.is_empty()
        && suffix.len() <= 4
        && suffix.chars().all(|c| c.is_ascii_lowercase())
        && suffix != "or"
}

/// What to say to an agent whose answer could not be spoken. It keeps
/// the capability by answering again, so this asks rather than scolds.
pub fn ask_again(reason: &str) -> String {
    format!(
        "That answer could not be read aloud, because {reason}. The user is listening, not \
         reading, so say it again in one or two sentences of plain speech, thirty words at \
         the most, with no code, no file paths, no URLs, no markdown and no lists. If the \
         detail needs any of those, say that it does and leave it in your terminal."
    )
}

/// What to tell the voice agent about an answer of its own. It has no
/// terminal to leave the detail in, so this asks for the part worth
/// hearing rather than for somewhere else to put the rest.
pub fn ask_for_less(reason: &str) -> String {
    format!(
        "That is not something the user can listen to, because {reason}. Say it again in one \
         or two sentences, thirty words at the most, keeping only what they asked for. If \
         there is more worth knowing, offer it in a few words and let them ask for it."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_answer_is_spoken_as_it_stands() {
        assert_eq!(trouble("I'm checking in phone 1000 now."), None);
        assert_eq!(trouble("Nothing's running. Tell me what to take on."), None);
    }

    /// The things that are unusable in a speaker, each named so the
    /// agent is told what to change rather than just refused.
    #[test]
    fn what_cannot_be_spoken_says_why() {
        assert!(trouble("Here: ```rust\nfn main() {}\n```")
            .unwrap()
            .contains("code block"));
        assert!(trouble("See https://example.com for it.")
            .unwrap()
            .contains("URL"));
        assert!(trouble("Fixed in crates/pm-daemon/src/voice/agent.rs now.")
            .unwrap()
            .contains("file path"));
        assert!(trouble("one\ntwo\nthree\nfour\nfive\nsix")
            .unwrap()
            .contains("lines"));
        assert!(trouble(&"word ".repeat(200)).unwrap().contains("too long"));
    }

    /// The two sentences an agent is asked for, at the length it would
    /// write them. A guard that sent this back would send everything
    /// back.
    #[test]
    fn two_spoken_sentences_are_inside_the_budget() {
        let answer = "I'm checking in phone one thousand now and the first two calls went \
                      through. I'll come back to you once the handover test finishes.";
        assert_eq!(answer.split_whitespace().count(), 24);
        assert_eq!(trouble(answer), None);
    }

    /// Long enough to listen to for a quarter of a minute, which is
    /// what the user hears as the agent being verbose.
    #[test]
    fn an_answer_too_long_to_listen_to_is_sent_back() {
        let answer = "So I went and looked at the femtocell and what I found was that the \
                      handover logic was holding the old reference for a little too long, \
                      which is why the second call dropped, and I've now changed it to \
                      release that reference as soon as the new one is up, and I'm running \
                      the suite again to see whether that was the whole story.";
        let reason = trouble(answer).expect("this is a paragraph, not an answer");
        assert!(reason.contains("words"), "{reason}");
        assert!(reason.contains("too long"), "{reason}");
    }

    /// Few words can still be a long time to listen to, so the
    /// character backstop catches what the word count misses.
    #[test]
    fn a_wall_of_identifiers_is_not_saved_by_its_word_count() {
        let answer = "a".repeat(400);
        assert!(trouble(&answer).unwrap().contains("characters"));
    }

    /// Both corrective rounds name what was wrong and the budget to
    /// answer within, because an agent told only "too long" guesses.
    #[test]
    fn being_asked_again_says_what_to_change() {
        for asked in [
            ask_again("it was 60 words"),
            ask_for_less("it was 60 words"),
        ] {
            assert!(asked.contains("it was 60 words"), "{asked}");
            assert!(asked.contains("thirty words"), "{asked}");
        }
        assert!(
            !ask_for_less("it was 60 words").contains("terminal"),
            "the voice agent has no terminal to leave anything in"
        );
    }

    /// Ordinary prose that happens to contain a slash is not a path. A
    /// guard that fires on "and/or" would send every answer back.
    #[test]
    fn prose_with_a_slash_is_not_a_path() {
        assert_eq!(trouble("I'll do it and/or ask first."), None);
        assert_eq!(trouble("It runs 24/7 now."), None);
    }
}
