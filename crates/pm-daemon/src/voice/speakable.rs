//! Whether an agent's answer can be read aloud as written.
//!
//! Replies are spoken exactly as the agent wrote them, so nothing here
//! rewrites one: it only says why a reply cannot be spoken, which is
//! handed back to the agent so it can answer again in words somebody
//! can listen to.

/// Longer than this is not an answer somebody listens to, it is a
/// document. Two spoken sentences land well inside it.
const SPOKEN_MAX: usize = 420;

/// More lines than this is a list however it is punctuated.
const LINES_MAX: usize = 4;

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
    let characters = text.chars().count();
    if characters > SPOKEN_MAX {
        return Some(format!(
            "it was {characters} characters, and anything over {SPOKEN_MAX} is too long to hear"
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
         reading, so say it again in at most two sentences of plain speech, with no code, no \
         file paths, no URLs, no markdown and no lists. If the detail needs any of those, say \
         that it does and leave it in your terminal."
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

    /// Ordinary prose that happens to contain a slash is not a path. A
    /// guard that fires on "and/or" would send every answer back.
    #[test]
    fn prose_with_a_slash_is_not_a_path() {
        assert_eq!(trouble("I'll do it and/or ask first."), None);
        assert_eq!(trouble("It runs 24/7 now."), None);
    }
}
