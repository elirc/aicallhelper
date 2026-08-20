//! Prompt construction (§7).
//!
//! Two rules govern every line in this file:
//!
//! 1. **The strings are product behavior.** They were tuned against real calls;
//!    paraphrasing them changes what the app says out loud. They are pinned by
//!    test, verbatim.
//! 2. **The cached prefix must be byte-stable.** Anthropic's prompt cache is a
//!    byte-prefix match, so anything non-deterministic (a timestamp, a hash-map
//!    iteration order) silently costs a cache write on every call. Everything
//!    here is a straight-line concatenation of owned inputs — no ordering, no
//!    clock, no environment.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnswerStyle {
    Brief,
    #[default]
    Balanced,
    Detailed,
}

impl AnswerStyle {
    pub fn as_str(self) -> &'static str {
        match self {
            AnswerStyle::Brief => "brief",
            AnswerStyle::Balanced => "balanced",
            AnswerStyle::Detailed => "detailed",
        }
    }

    /// Unknown / corrupt style falls back to balanced (§7). The settings file is
    /// user-writable, so this is a real input path, not a theoretical one.
    pub fn parse_or_default(raw: &str) -> Self {
        match raw {
            "brief" => AnswerStyle::Brief,
            "detailed" => AnswerStyle::Detailed,
            _ => AnswerStyle::Balanced,
        }
    }
}

pub const ROLE_INSTRUCTIONS: &str = "You are a real-time call assistant helping the user answer questions asked of them during a live interview or call. You are given a transcript of what the other person just said. Reply with the answer the user should say, written in first person, in natural spoken English. Do not add meta commentary, greetings, or quotation marks — output only the answer itself. If the transcript contains no real question, briefly suggest what the user could say next.";

pub const RESUME_HEADER: &str = "\n\n--- THE USER'S RESUME ---\n";
pub const JD_HEADER: &str = "\n\n--- THE JOB THEY ARE INTERVIEWING FOR ---\n";
pub const GROUNDING_NOTE: &str = "\n\nGround every answer in the resume and target role above. Never invent experience the resume does not support.";

pub const STYLE_BRIEF: &str = "Answer in one or two spoken sentences — the shortest reply that fully answers the question. No lists, no headings, no lead-in.";
pub const STYLE_BALANCED: &str = "Be concise and confident: a few sentences for simple questions, short structured points for complex ones.";
pub const STYLE_DETAILED: &str = "Give a structured answer: one sentence that answers directly, then three to five short supporting points (what the situation was, what you did, what the result was). Keep every point short enough to say in one breath — this is spoken aloud, not read.";

/// The user's saved profile. Held by reference so building a prompt never
/// clones the (potentially 200 KB) resume more than once.
#[derive(Debug, Clone, Copy, Default)]
pub struct Profile<'a> {
    pub resume: &'a str,
    pub job_description: &'a str,
}

/// The system prompt, split at the cache breakpoint.
///
/// `cached_prefix` holds the role instructions plus the resume/JD — the large,
/// stable part worth caching. `style_suffix` sits *after* the breakpoint so
/// flipping answer style never invalidates the cached profile (§3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemPrompt {
    pub cached_prefix: String,
    pub style_suffix: String,
}

impl SystemPrompt {
    /// Groq takes a single system string; joining with a blank line keeps the
    /// two halves visually distinct to the model, exactly as the two Anthropic
    /// blocks read.
    pub fn joined(&self) -> String {
        format!("{}\n\n{}", self.cached_prefix, self.style_suffix)
    }
}

pub fn build_system_prompt(profile: Profile<'_>, style: AnswerStyle) -> SystemPrompt {
    let mut prefix = String::with_capacity(
        ROLE_INSTRUCTIONS.len() + profile.resume.len() + profile.job_description.len() + 256,
    );
    prefix.push_str(ROLE_INSTRUCTIONS);

    // Trim only at the edges: interior formatting of a resume (indentation,
    // blank lines between roles) is meaningful and survives verbatim (§7).
    let resume = profile.resume.trim();
    let jd = profile.job_description.trim();

    if !resume.is_empty() {
        prefix.push_str(RESUME_HEADER);
        prefix.push_str(resume);
    }
    if !jd.is_empty() {
        prefix.push_str(JD_HEADER);
        prefix.push_str(jd);
    }
    // The grounding note is appended if *either* section was present — with no
    // profile at all there is nothing to ground against and the sentence would
    // be a lie the model then tries to obey.
    if !resume.is_empty() || !jd.is_empty() {
        prefix.push_str(GROUNDING_NOTE);
    }

    let style_suffix = match style {
        AnswerStyle::Brief => STYLE_BRIEF,
        AnswerStyle::Balanced => STYLE_BALANCED,
        AnswerStyle::Detailed => STYLE_DETAILED,
    };

    SystemPrompt { cached_prefix: prefix, style_suffix: style_suffix.to_string() }
}

/// The user turn (§7). The transcript is wrapped rather than concatenated so
/// the model can tell speech from instruction, and it lives outside the system
/// prompt so the cached prefix stays identical across questions.
pub fn build_user_message(transcript: &str) -> String {
    format!("The other person on the call just said:\n\"\"\"\n{transcript}\n\"\"\"\n\nWhat should I say?")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_instructions_are_verbatim() {
        // Pinned because this text is the product's voice. If someone "improves"
        // the wording, that is a product decision and should break a test.
        assert_eq!(
            ROLE_INSTRUCTIONS,
            "You are a real-time call assistant helping the user answer questions asked of them during a live interview or call. You are given a transcript of what the other person just said. Reply with the answer the user should say, written in first person, in natural spoken English. Do not add meta commentary, greetings, or quotation marks — output only the answer itself. If the transcript contains no real question, briefly suggest what the user could say next."
        );
    }

    #[test]
    fn empty_profile_yields_role_instructions_only_and_no_grounding_note() {
        // The grounding note would instruct the model to ground in a resume that
        // is not there, which reliably produces "based on your background..."
        // hedging about nothing.
        let p = build_system_prompt(Profile::default(), AnswerStyle::Balanced);
        assert_eq!(p.cached_prefix, ROLE_INSTRUCTIONS);
        assert!(!p.cached_prefix.contains("Ground every answer"));
    }

    #[test]
    fn whitespace_only_profile_counts_as_absent() {
        let p = build_system_prompt(
            Profile { resume: "   \n\t ", job_description: "\n\n" },
            AnswerStyle::Balanced,
        );
        assert_eq!(p.cached_prefix, ROLE_INSTRUCTIONS);
    }

    #[test]
    fn resume_only_still_gets_the_grounding_note() {
        let p = build_system_prompt(
            Profile { resume: "Ten years of Rust.", job_description: "" },
            AnswerStyle::Balanced,
        );
        assert_eq!(
            p.cached_prefix,
            format!("{ROLE_INSTRUCTIONS}{RESUME_HEADER}Ten years of Rust.{GROUNDING_NOTE}")
        );
    }

    #[test]
    fn jd_only_still_gets_the_grounding_note() {
        let p = build_system_prompt(
            Profile { resume: "", job_description: "Staff Engineer, payments." },
            AnswerStyle::Balanced,
        );
        assert_eq!(
            p.cached_prefix,
            format!("{ROLE_INSTRUCTIONS}{JD_HEADER}Staff Engineer, payments.{GROUNDING_NOTE}")
        );
    }

    #[test]
    fn sections_appear_in_resume_then_jd_order() {
        let p = build_system_prompt(
            Profile { resume: "R", job_description: "J" },
            AnswerStyle::Balanced,
        );
        let r = p.cached_prefix.find("--- THE USER'S RESUME ---").unwrap();
        let j = p.cached_prefix.find("--- THE JOB THEY ARE INTERVIEWING FOR ---").unwrap();
        assert!(r < j, "resume must precede the job description for cache stability");
    }

    #[test]
    fn interior_resume_formatting_survives_verbatim() {
        // Only the edges are trimmed. A resume's internal blank lines and
        // indentation carry meaning the model reads.
        let resume = "  Line one\n\n    indented\n  ";
        let p = build_system_prompt(
            Profile { resume, job_description: "" },
            AnswerStyle::Balanced,
        );
        assert!(p.cached_prefix.contains("Line one\n\n    indented"));
        assert!(!p.cached_prefix.contains("Line one\n\n    indented\n  "));
    }

    #[test]
    fn style_lives_outside_the_cached_prefix() {
        // This is the whole point of the split: flipping style must not change
        // one byte of the cached prefix, or every style toggle costs a cache
        // write plus a full re-read of the profile (§3).
        let profile = Profile { resume: "R", job_description: "J" };
        let brief = build_system_prompt(profile, AnswerStyle::Brief);
        let balanced = build_system_prompt(profile, AnswerStyle::Balanced);
        let detailed = build_system_prompt(profile, AnswerStyle::Detailed);

        assert_eq!(brief.cached_prefix, balanced.cached_prefix);
        assert_eq!(balanced.cached_prefix, detailed.cached_prefix);
        assert_ne!(brief.style_suffix, balanced.style_suffix);
        assert_ne!(balanced.style_suffix, detailed.style_suffix);

        // And no style text leaks into the prefix.
        assert!(!balanced.cached_prefix.contains("Be concise and confident"));
    }

    #[test]
    fn style_suffixes_are_verbatim() {
        assert_eq!(build_system_prompt(Profile::default(), AnswerStyle::Brief).style_suffix, STYLE_BRIEF);
        assert_eq!(build_system_prompt(Profile::default(), AnswerStyle::Balanced).style_suffix, STYLE_BALANCED);
        assert_eq!(build_system_prompt(Profile::default(), AnswerStyle::Detailed).style_suffix, STYLE_DETAILED);
        assert_eq!(STYLE_BRIEF, "Answer in one or two spoken sentences — the shortest reply that fully answers the question. No lists, no headings, no lead-in.");
        assert_eq!(STYLE_BALANCED, "Be concise and confident: a few sentences for simple questions, short structured points for complex ones.");
        assert_eq!(STYLE_DETAILED, "Give a structured answer: one sentence that answers directly, then three to five short supporting points (what the situation was, what you did, what the result was). Keep every point short enough to say in one breath — this is spoken aloud, not read.");
    }

    #[test]
    fn prompt_is_byte_stable_across_repeated_builds() {
        // Cache hits are a byte-prefix match. If this ever fails, something
        // non-deterministic crept in and the cache silently stopped paying.
        let profile = Profile { resume: "R\nmulti\nline", job_description: "J" };
        let first = build_system_prompt(profile, AnswerStyle::Detailed);
        for _ in 0..50 {
            assert_eq!(build_system_prompt(profile, AnswerStyle::Detailed), first);
        }
    }

    #[test]
    fn unknown_style_falls_back_to_balanced() {
        assert_eq!(AnswerStyle::parse_or_default("brief"), AnswerStyle::Brief);
        assert_eq!(AnswerStyle::parse_or_default("detailed"), AnswerStyle::Detailed);
        assert_eq!(AnswerStyle::parse_or_default("balanced"), AnswerStyle::Balanced);
        // Corrupt / hand-edited settings file values.
        assert_eq!(AnswerStyle::parse_or_default(""), AnswerStyle::Balanced);
        assert_eq!(AnswerStyle::parse_or_default("BRIEF"), AnswerStyle::Balanced);
        assert_eq!(AnswerStyle::parse_or_default("verbose"), AnswerStyle::Balanced);
    }

    #[test]
    fn user_message_wrapper_is_verbatim() {
        assert_eq!(
            build_user_message("Tell me about yourself."),
            "The other person on the call just said:\n\"\"\"\nTell me about yourself.\n\"\"\"\n\nWhat should I say?"
        );
    }

    #[test]
    fn transcript_is_not_escaped_or_trimmed_by_the_wrapper() {
        // The transcript is data, not markup. Deepgram can emit quotes; mangling
        // them here would change the question the model answers.
        let msg = build_user_message("She said \"hello\" — then paused");
        assert!(msg.contains("She said \"hello\" — then paused"));
    }

    #[test]
    fn joined_prompt_places_style_after_the_prefix() {
        let p = build_system_prompt(Profile { resume: "R", job_description: "" }, AnswerStyle::Brief);
        let joined = p.joined();
        assert!(joined.starts_with(&p.cached_prefix));
        assert!(joined.ends_with(&p.style_suffix));
        assert_eq!(joined, format!("{}\n\n{}", p.cached_prefix, p.style_suffix));
    }
}
