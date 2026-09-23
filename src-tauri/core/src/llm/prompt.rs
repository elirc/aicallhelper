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
//!
//! Call profiles (v3.1, ADR 014) were added under a third rule: **every new
//! section is a new constant beside the pinned ones, never an edit.** An
//! interview profile with no focus and no extra instructions builds a prefix
//! byte-identical to v3, so the upgrade changes nothing the app says and costs
//! no existing user a cache write.

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

/// What kind of call a profile is for (§7). A closed enum on purpose: a
/// free-text call type would be unpinnable prompt text headed straight for
/// the system block. `Interview` is the v3 behavior and the migration default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CallType {
    #[default]
    Interview,
    Sales,
    Support,
    Meeting,
    Other,
}

impl CallType {
    pub fn as_str(self) -> &'static str {
        match self {
            CallType::Interview => "interview",
            CallType::Sales => "sales",
            CallType::Support => "support",
            CallType::Meeting => "meeting",
            CallType::Other => "other",
        }
    }

    /// Unknown / corrupt value falls back to interview — exactly v3's behavior,
    /// so a hand-edited or downgraded-then-upgraded file never changes how a
    /// profile is framed (§8).
    pub fn parse_or_default(raw: &str) -> Self {
        match raw {
            "sales" => CallType::Sales,
            "support" => CallType::Support,
            "meeting" => CallType::Meeting,
            "other" => CallType::Other,
            _ => CallType::Interview,
        }
    }
}

pub const ROLE_INSTRUCTIONS: &str = "You are a real-time call assistant helping the user answer questions asked of them during a live interview or call. You are given a transcript of what the other person just said. Reply with the answer the user should say, written in first person, in natural spoken English. Do not add meta commentary, greetings, or quotation marks — output only the answer itself. If the transcript contains no real question, briefly suggest what the user could say next.";

pub const RESUME_HEADER: &str = "\n\n--- THE USER'S RESUME ---\n";
pub const JD_HEADER: &str = "\n\n--- THE JOB THEY ARE INTERVIEWING FOR ---\n";
pub const GROUNDING_NOTE: &str = "\n\nGround every answer in the resume and target role above. Never invent experience the resume does not support.";

// Call-type framing (§7, ADR 014): one pinned sentence per non-interview call
// type, placed directly after ROLE_INSTRUCTIONS inside the cached prefix.
// Interview has none — that absence is what keeps a migrated v3 profile
// byte-identical.
pub const CALL_TYPE_SALES: &str = "\n\nThis is a sales call: the user is selling to the other person. Answer as the user speaking to a prospect or customer — specific, helpful, and never pushy.";
pub const CALL_TYPE_SUPPORT: &str = "\n\nThis is a customer support call: the user is helping the other person. Answer as the user speaking to a customer — calm, clear, and focused on resolving their issue.";
pub const CALL_TYPE_MEETING: &str = "\n\nThis is a work meeting: the user is a participant, not a candidate. Answer as the user speaking to colleagues — direct and to the point.";
pub const CALL_TYPE_OTHER: &str = "\n\nThis is a general call, not a job interview. Answer as the user speaking to the other person.";

// Non-interview profiles reuse the resume / JD slots as "about the user" and
// "context for this call", with a grounding note that names neither a resume
// nor a target role — the interview wording would tell a sales rep to ground
// answers in a job they are not interviewing for.
pub const BACKGROUND_HEADER: &str = "\n\n--- ABOUT THE USER ---\n";
pub const CONTEXT_HEADER: &str = "\n\n--- CONTEXT FOR THIS CALL ---\n";
pub const GROUNDING_NOTE_CALL: &str = "\n\nGround every answer in the background and call context above. Never invent experience or facts the background does not support.";

// Optional sections shared by every call type, appended after the grounding
// note so they can never change the bytes of a profile that does not use them.
pub const FOCUS_HEADER: &str = "\n\n--- WHAT TO EMPHASIZE ---\n";
pub const EXTRA_INSTRUCTIONS_HEADER: &str = "\n\n--- ADDITIONAL INSTRUCTIONS FROM THE USER ---\n";

pub const STYLE_BRIEF: &str = "Answer in one or two spoken sentences — the shortest reply that fully answers the question. No lists, no headings, no lead-in.";
pub const STYLE_BALANCED: &str = "Be concise and confident: a few sentences for simple questions, short structured points for complex ones.";
pub const STYLE_DETAILED: &str = "Give a structured answer: one sentence that answers directly, then three to five short supporting points (what the situation was, what you did, what the result was). Keep every point short enough to say in one breath — this is spoken aloud, not read.";

/// The active call profile's text (§7). Held by reference so building a
/// prompt never clones the (potentially 200 KB) resume more than once.
///
/// `Default` is exactly a v3 profile — interview, everything empty — so
/// `Profile { resume, job_description, ..Default::default() }` spells the
/// pre-profiles prompt and the byte-identity test below can pin it.
#[derive(Debug, Clone, Copy, Default)]
pub struct Profile<'a> {
    pub call_type: CallType,
    pub resume: &'a str,
    /// The job description for an interview; the call context (account,
    /// product, agenda) for every other call type.
    pub job_description: &'a str,
    pub focus: &'a str,
    pub extra_instructions: &'a str,
}

/// The system prompt, split at the cache breakpoint.
///
/// `cached_prefix` holds the role instructions plus the whole profile (call
/// framing, resume / JD, grounding, focus, extra instructions) — the large,
/// stable-per-call part worth caching. `style_suffix` sits *after* the
/// breakpoint so flipping answer style never invalidates the cached profile
/// (§3).
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

/// The header set for a call type: (call line, resume header, JD header,
/// grounding note). A straight-line match — no map, no clock — so the prefix
/// stays byte-stable (ADR 007). Interview returns v3's exact set with an
/// empty call line.
fn sections_for(call_type: CallType) -> (&'static str, &'static str, &'static str, &'static str) {
    match call_type {
        CallType::Interview => ("", RESUME_HEADER, JD_HEADER, GROUNDING_NOTE),
        CallType::Sales => (CALL_TYPE_SALES, BACKGROUND_HEADER, CONTEXT_HEADER, GROUNDING_NOTE_CALL),
        CallType::Support => (CALL_TYPE_SUPPORT, BACKGROUND_HEADER, CONTEXT_HEADER, GROUNDING_NOTE_CALL),
        CallType::Meeting => (CALL_TYPE_MEETING, BACKGROUND_HEADER, CONTEXT_HEADER, GROUNDING_NOTE_CALL),
        CallType::Other => (CALL_TYPE_OTHER, BACKGROUND_HEADER, CONTEXT_HEADER, GROUNDING_NOTE_CALL),
    }
}

pub fn build_system_prompt(profile: Profile<'_>, style: AnswerStyle) -> SystemPrompt {
    let (call_line, resume_header, jd_header, grounding) = sections_for(profile.call_type);

    // Trim only at the edges: interior formatting of a resume (indentation,
    // blank lines between roles) is meaningful and survives verbatim (§7).
    let resume = profile.resume.trim();
    let jd = profile.job_description.trim();
    let focus = profile.focus.trim();
    let extra = profile.extra_instructions.trim();

    let mut prefix = String::with_capacity(
        ROLE_INSTRUCTIONS.len()
            + call_line.len()
            + resume.len()
            + jd.len()
            + focus.len()
            + extra.len()
            + 512,
    );
    prefix.push_str(ROLE_INSTRUCTIONS);
    // "" for interview: a migrated v3 profile appends nothing here.
    prefix.push_str(call_line);

    if !resume.is_empty() {
        prefix.push_str(resume_header);
        prefix.push_str(resume);
    }
    if !jd.is_empty() {
        prefix.push_str(jd_header);
        prefix.push_str(jd);
    }
    // The grounding note is appended if *either* section was present — with no
    // profile at all there is nothing to ground against and the sentence would
    // be a lie the model then tries to obey. Focus alone does NOT count: a
    // list of things to emphasize is a steer, not a background to ground in.
    if !resume.is_empty() || !jd.is_empty() {
        prefix.push_str(grounding);
    }
    if !focus.is_empty() {
        prefix.push_str(FOCUS_HEADER);
        prefix.push_str(focus);
    }
    if !extra.is_empty() {
        prefix.push_str(EXTRA_INSTRUCTIONS_HEADER);
        prefix.push_str(extra);
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

/// Bytes of the request a provider that sends `system` and `transcript` as
/// two messages carries: the joined system prompt plus the wrapped user turn.
/// This is THE count the local provider's gate enforces (`local::request_body`
/// calls it), so a preview built on it cannot drift from the gate.
pub fn request_input_bytes(system: &SystemPrompt, transcript: &str) -> usize {
    system.joined().len() + build_user_message(transcript).len()
}

/// The room a question should leave for the average spoken question. A
/// usability line for the "little room" warning, NOT a limit: the only hard
/// limit is `local::MAX_INPUT_BYTES` (FINAL-REVIEW §4).
pub const QUESTION_RESERVE_BYTES: usize = 200;

/// How a local request of this shape sits against the limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BudgetStatus {
    /// At least `QUESTION_RESERVE_BYTES` left.
    Ok,
    /// Fits, but fewer than `QUESTION_RESERVE_BYTES` left: many spoken
    /// questions will not fit. A warning; the profile stays usable.
    Tight,
    /// The request exceeds the limit (with no question yet: not even a
    /// one-byte question fits). Local answers with it are refused.
    Over,
}

/// The local prompt budget for one profile + style + question (R4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalPromptBudget {
    /// Exactly what `local::request_body` would count for this request.
    pub used_bytes: usize,
    /// `local::MAX_INPUT_BYTES`.
    pub limit_bytes: usize,
    /// `limit_bytes - used_bytes`; negative when over.
    pub remaining_bytes: i64,
    /// Everything that is not the user's text: role instructions, call line,
    /// section headers, grounding note, the blank-line join, the style suffix
    /// and the question wrapper.
    pub fixed_bytes: usize,
    /// The profile fields as the builder uses them (edge-trimmed; an empty
    /// field contributes nothing).
    pub profile_bytes: usize,
    /// The question as passed, untrimmed (the wrapper does not trim).
    pub question_bytes: usize,
    pub reserve_bytes: usize,
    pub status: BudgetStatus,
}

/// The local request budget, computed by building the request exactly as the
/// answer path does. Never duplicated in TypeScript: the Settings preview,
/// the pre-record check and the typed-Ask check all come through here.
///
/// An empty `question` asks "what is left for a question?". It counts as
/// `Over` when not even a one-byte question fits, because no real question
/// is empty.
pub fn local_prompt_budget(profile: Profile<'_>, style: AnswerStyle, question: &str) -> LocalPromptBudget {
    let limit = super::local::MAX_INPUT_BYTES;
    let used = request_input_bytes(&build_system_prompt(profile, style), question);
    let profile_bytes = profile.resume.trim().len()
        + profile.job_description.trim().len()
        + profile.focus.trim().len()
        + profile.extra_instructions.trim().len();
    let remaining = limit as i64 - used as i64;
    let over = if question.is_empty() { remaining < 1 } else { remaining < 0 };
    let status = if over {
        BudgetStatus::Over
    } else if remaining < QUESTION_RESERVE_BYTES as i64 {
        BudgetStatus::Tight
    } else {
        BudgetStatus::Ok
    };
    LocalPromptBudget {
        used_bytes: used,
        limit_bytes: limit,
        remaining_bytes: remaining,
        fixed_bytes: used - profile_bytes - question.len(),
        profile_bytes,
        question_bytes: question.len(),
        reserve_bytes: QUESTION_RESERVE_BYTES,
        status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A profile using every section, for the tests that must prove the new
    /// sections behave exactly like the old ones (style split, byte stability).
    fn full_sales_profile() -> Profile<'static> {
        Profile {
            call_type: CallType::Sales,
            resume: "R\nmulti\nline",
            job_description: "J",
            focus: "F",
            extra_instructions: "E",
        }
    }

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
            Profile { resume: "   \n\t ", job_description: "\n\n", ..Default::default() },
            AnswerStyle::Balanced,
        );
        assert_eq!(p.cached_prefix, ROLE_INSTRUCTIONS);
    }

    #[test]
    fn resume_only_still_gets_the_grounding_note() {
        let p = build_system_prompt(
            Profile { resume: "Ten years of Rust.", job_description: "", ..Default::default() },
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
            Profile { resume: "", job_description: "Staff Engineer, payments.", ..Default::default() },
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
            Profile { resume: "R", job_description: "J", ..Default::default() },
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
            Profile { resume, job_description: "", ..Default::default() },
            AnswerStyle::Balanced,
        );
        assert!(p.cached_prefix.contains("Line one\n\n    indented"));
        assert!(!p.cached_prefix.contains("Line one\n\n    indented\n  "));
    }

    #[test]
    fn style_lives_outside_the_cached_prefix() {
        // This is the whole point of the split: flipping style must not change
        // one byte of the cached prefix, or every style toggle costs a cache
        // write plus a full re-read of the profile (§3). Checked for the v3
        // interview shape AND a profile using every new section, so a new
        // section can never be accidentally routed after the breakpoint.
        for profile in [
            Profile { resume: "R", job_description: "J", ..Default::default() },
            full_sales_profile(),
        ] {
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
        // The full Sales profile exercises every branch of the builder.
        for profile in [
            Profile { resume: "R\nmulti\nline", job_description: "J", ..Default::default() },
            full_sales_profile(),
        ] {
            let first = build_system_prompt(profile, AnswerStyle::Detailed);
            for _ in 0..50 {
                assert_eq!(build_system_prompt(profile, AnswerStyle::Detailed), first);
            }
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
        let p = build_system_prompt(
            Profile { resume: "R", job_description: "", ..Default::default() },
            AnswerStyle::Brief,
        );
        let joined = p.joined();
        assert!(joined.starts_with(&p.cached_prefix));
        assert!(joined.ends_with(&p.style_suffix));
        assert_eq!(joined, format!("{}\n\n{}", p.cached_prefix, p.style_suffix));
    }

    // ----------------------------------------------------- call profiles ----

    #[test]
    fn migrated_v3_profile_yields_a_byte_identical_prefix() {
        // The upgrade must not change what the app says and must not cost every
        // existing user a cache write: interview + no focus + no extra == the
        // v3 bytes exactly (ADR 007). Spelled out from the pinned constants so
        // a new section slipping into the interview path breaks this.
        let p = build_system_prompt(
            Profile { resume: "R", job_description: "J", ..Default::default() },
            AnswerStyle::Balanced,
        );
        assert_eq!(p.cached_prefix, format!("{ROLE_INSTRUCTIONS}{RESUME_HEADER}R{JD_HEADER}J{GROUNDING_NOTE}"));

        // The migration profile is the Default profile — the two must be the
        // same bytes, or `..Default::default()` in this file would be lying.
        let explicit = build_system_prompt(
            Profile {
                call_type: CallType::Interview,
                resume: "R",
                job_description: "J",
                focus: "",
                extra_instructions: "",
            },
            AnswerStyle::Balanced,
        );
        assert_eq!(explicit, p);
    }

    #[test]
    fn call_type_lines_and_new_headers_are_verbatim() {
        // Pinned like ROLE_INSTRUCTIONS: these sentences are what a sales or
        // support user hears the app say. Nine constants, all of them.
        assert_eq!(CALL_TYPE_SALES, "\n\nThis is a sales call: the user is selling to the other person. Answer as the user speaking to a prospect or customer — specific, helpful, and never pushy.");
        assert_eq!(CALL_TYPE_SUPPORT, "\n\nThis is a customer support call: the user is helping the other person. Answer as the user speaking to a customer — calm, clear, and focused on resolving their issue.");
        assert_eq!(CALL_TYPE_MEETING, "\n\nThis is a work meeting: the user is a participant, not a candidate. Answer as the user speaking to colleagues — direct and to the point.");
        assert_eq!(CALL_TYPE_OTHER, "\n\nThis is a general call, not a job interview. Answer as the user speaking to the other person.");
        assert_eq!(BACKGROUND_HEADER, "\n\n--- ABOUT THE USER ---\n");
        assert_eq!(CONTEXT_HEADER, "\n\n--- CONTEXT FOR THIS CALL ---\n");
        assert_eq!(GROUNDING_NOTE_CALL, "\n\nGround every answer in the background and call context above. Never invent experience or facts the background does not support.");
        assert_eq!(FOCUS_HEADER, "\n\n--- WHAT TO EMPHASIZE ---\n");
        assert_eq!(EXTRA_INSTRUCTIONS_HEADER, "\n\n--- ADDITIONAL INSTRUCTIONS FROM THE USER ---\n");

        // And the v3 strings did not move an inch.
        assert_eq!(RESUME_HEADER, "\n\n--- THE USER'S RESUME ---\n");
        assert_eq!(JD_HEADER, "\n\n--- THE JOB THEY ARE INTERVIEWING FOR ---\n");
        assert_eq!(GROUNDING_NOTE, "\n\nGround every answer in the resume and target role above. Never invent experience the resume does not support.");
    }

    #[test]
    fn interview_emits_no_call_type_line() {
        let p = build_system_prompt(
            Profile { call_type: CallType::Interview, resume: "R", ..Default::default() },
            AnswerStyle::Balanced,
        );
        for line in [CALL_TYPE_SALES, CALL_TYPE_SUPPORT, CALL_TYPE_MEETING, CALL_TYPE_OTHER] {
            assert!(!p.cached_prefix.contains(line));
        }
        // The resume header follows the role text immediately, as in v3.
        assert!(p.cached_prefix.starts_with(&format!("{ROLE_INSTRUCTIONS}{RESUME_HEADER}")));
    }

    #[test]
    fn non_interview_uses_background_context_headers_and_call_grounding_note() {
        // Every non-interview type swaps the whole header set: the interview
        // wording ("the job they are interviewing for") must never reach a
        // sales, support, meeting or general-call profile.
        for (call_type, line) in [
            (CallType::Sales, CALL_TYPE_SALES),
            (CallType::Support, CALL_TYPE_SUPPORT),
            (CallType::Meeting, CALL_TYPE_MEETING),
            (CallType::Other, CALL_TYPE_OTHER),
        ] {
            let p = build_system_prompt(
                Profile { call_type, resume: "R", job_description: "J", ..Default::default() },
                AnswerStyle::Balanced,
            );
            assert_eq!(
                p.cached_prefix,
                format!("{ROLE_INSTRUCTIONS}{line}{BACKGROUND_HEADER}R{CONTEXT_HEADER}J{GROUNDING_NOTE_CALL}"),
                "{call_type:?}"
            );
            assert!(!p.cached_prefix.contains(RESUME_HEADER), "{call_type:?}");
            assert!(!p.cached_prefix.contains(JD_HEADER), "{call_type:?}");
            assert!(!p.cached_prefix.contains(GROUNDING_NOTE), "{call_type:?}");
        }
    }

    #[test]
    fn sections_appear_in_call_resume_jd_grounding_focus_extra_order() {
        let p = build_system_prompt(full_sales_profile(), AnswerStyle::Balanced);
        let at = |s: &str| p.cached_prefix.find(s).unwrap();
        assert!(p.cached_prefix.starts_with(ROLE_INSTRUCTIONS));
        assert!(at(CALL_TYPE_SALES) < at(BACKGROUND_HEADER));
        assert!(at(BACKGROUND_HEADER) < at(CONTEXT_HEADER));
        assert!(at(CONTEXT_HEADER) < at(GROUNDING_NOTE_CALL));
        assert!(at(GROUNDING_NOTE_CALL) < at(FOCUS_HEADER));
        assert!(at(FOCUS_HEADER) < at(EXTRA_INSTRUCTIONS_HEADER));
        // Pinned end to end, so the order can never drift by one section.
        assert_eq!(
            p.cached_prefix,
            format!("{ROLE_INSTRUCTIONS}{CALL_TYPE_SALES}{BACKGROUND_HEADER}R\nmulti\nline{CONTEXT_HEADER}J{GROUNDING_NOTE_CALL}{FOCUS_HEADER}F{EXTRA_INSTRUCTIONS_HEADER}E")
        );
        assert!(!p.cached_prefix.contains(RESUME_HEADER));
        assert!(!p.cached_prefix.contains(JD_HEADER));
        assert!(!p.cached_prefix.contains(GROUNDING_NOTE));
    }

    #[test]
    fn whitespace_only_focus_and_extra_count_as_absent() {
        // Same edge rule as the resume: a stray newline in an optional field
        // must not emit an empty section header the model then puzzles over.
        let p = build_system_prompt(
            Profile {
                call_type: CallType::Sales,
                resume: "R",
                focus: " \n\t",
                extra_instructions: "\n\n",
                ..Default::default()
            },
            AnswerStyle::Balanced,
        );
        assert!(!p.cached_prefix.contains(FOCUS_HEADER));
        assert!(!p.cached_prefix.contains(EXTRA_INSTRUCTIONS_HEADER));
        assert_eq!(
            p.cached_prefix,
            format!("{ROLE_INSTRUCTIONS}{CALL_TYPE_SALES}{BACKGROUND_HEADER}R{GROUNDING_NOTE_CALL}")
        );
    }

    #[test]
    fn focus_alone_does_not_trigger_the_grounding_note() {
        // A focus line is a steer, not something to ground in: "ground every
        // answer in the background above" with no background is the same lie
        // as the empty-profile case, and the model hedges about it the same way.
        let p = build_system_prompt(
            Profile { focus: "Rust, tokio", ..Default::default() },
            AnswerStyle::Balanced,
        );
        assert_eq!(p.cached_prefix, format!("{ROLE_INSTRUCTIONS}{FOCUS_HEADER}Rust, tokio"));
        assert!(!p.cached_prefix.contains("Ground every answer"));

        let sales = build_system_prompt(
            Profile { call_type: CallType::Sales, focus: "F", extra_instructions: "E", ..Default::default() },
            AnswerStyle::Balanced,
        );
        assert_eq!(
            sales.cached_prefix,
            format!("{ROLE_INSTRUCTIONS}{CALL_TYPE_SALES}{FOCUS_HEADER}F{EXTRA_INSTRUCTIONS_HEADER}E")
        );
        assert!(!sales.cached_prefix.contains("Ground every answer"));
    }

    #[test]
    fn unknown_call_type_falls_back_to_interview() {
        assert_eq!(CallType::parse_or_default("interview"), CallType::Interview);
        assert_eq!(CallType::parse_or_default("sales"), CallType::Sales);
        assert_eq!(CallType::parse_or_default("support"), CallType::Support);
        assert_eq!(CallType::parse_or_default("meeting"), CallType::Meeting);
        assert_eq!(CallType::parse_or_default("other"), CallType::Other);
        // Corrupt / hand-edited settings file values — v3's framing, never a
        // guess at a different call type.
        assert_eq!(CallType::parse_or_default(""), CallType::Interview);
        assert_eq!(CallType::parse_or_default("SALES"), CallType::Interview);
        assert_eq!(CallType::parse_or_default("persona"), CallType::Interview);
        assert_eq!(CallType::default(), CallType::Interview);

        // as_str and the serde wire form agree with parse_or_default, so a
        // value that survives a save/load round trip is the same value.
        for call_type in [CallType::Interview, CallType::Sales, CallType::Support, CallType::Meeting, CallType::Other] {
            assert_eq!(CallType::parse_or_default(call_type.as_str()), call_type);
            assert_eq!(serde_json::to_string(&call_type).unwrap(), format!("\"{}\"", call_type.as_str()));
        }
    }

    // --- R4: the local prompt budget ---------------------------------------

    use crate::llm::local::{request_body, MAX_INPUT_BYTES};
    use crate::llm::AnswerRequest;

    const ALL_CALL_TYPES: [CallType; 5] =
        [CallType::Interview, CallType::Sales, CallType::Support, CallType::Meeting, CallType::Other];
    const ALL_STYLES: [AnswerStyle; 3] = [AnswerStyle::Brief, AnswerStyle::Balanced, AnswerStyle::Detailed];

    /// The bytes the local gate actually measures, read back from the body
    /// it builds (or `None` when the gate refuses).
    fn gate_bytes(profile: Profile<'_>, style: AnswerStyle, question: &str) -> Option<usize> {
        let req = AnswerRequest::new(build_system_prompt(profile, style)).with_transcript(question);
        let body = request_body(&req).ok()?;
        let len = |i: usize| body["messages"][i]["content"].as_str().unwrap().len();
        Some(len(0) + len(1))
    }

    /// A question of exactly `n` ASCII bytes.
    fn ascii(n: usize) -> String {
        "q".repeat(n)
    }

    #[test]
    fn budget_fixed_overhead_for_interview_balanced_resume_only_is_771() {
        // The maintainer's hand count (role 457, resume header 28, grounding
        // 111, join 2, Balanced 105, wrapper 68), verified rather than
        // trusted, plus the review's reproduction: a 6,500-byte resume and
        // "Hi?" is 7,274 bytes, over the cap the old 6,500 warning missed.
        let resume = "x".repeat(6_500);
        let profile = Profile { resume: &resume, ..Default::default() };
        let b = local_prompt_budget(profile, AnswerStyle::Balanced, "Hi?");
        assert_eq!(b.fixed_bytes, 771);
        assert_eq!(b.profile_bytes, 6_500);
        assert_eq!(b.question_bytes, 3);
        assert_eq!(b.used_bytes, 7_274);
        assert_eq!(b.remaining_bytes, -274);
        assert_eq!(b.status, BudgetStatus::Over);
        assert_eq!(gate_bytes(profile, AnswerStyle::Balanced, "Hi?"), None, "and the gate agrees");
        // The biggest resume that leaves any room at all for this shape.
        let fits = "x".repeat(MAX_INPUT_BYTES - 771 - 1);
        let b = local_prompt_budget(Profile { resume: &fits, ..Default::default() }, AnswerStyle::Balanced, "");
        assert_eq!((b.remaining_bytes, b.status), (1, BudgetStatus::Tight));
    }

    #[test]
    fn budget_equals_the_gate_for_every_call_type_style_and_field_combination() {
        // 5 call types x 3 styles x 16 optional-field combinations: the
        // preview must count exactly what the gate counts, whatever sections
        // the builder adds or leaves out.
        for call_type in ALL_CALL_TYPES {
            for style in ALL_STYLES {
                for mask in 0u8..16 {
                    let pick = |bit: u8, text: &'static str| if mask & bit != 0 { text } else { "" };
                    let profile = Profile {
                        call_type,
                        resume: pick(1, "Resume text"),
                        job_description: pick(2, "Role text"),
                        focus: pick(4, "Rust"),
                        extra_instructions: pick(8, "Be brief"),
                    };
                    let b = local_prompt_budget(profile, style, "What is REST?");
                    let case = format!("{call_type:?}/{style:?}/{mask:04b}");
                    assert_eq!(Some(b.used_bytes), gate_bytes(profile, style, "What is REST?"), "{case}");
                    assert_eq!(b.used_bytes, b.fixed_bytes + b.profile_bytes + b.question_bytes, "{case}");
                    assert_eq!(b.limit_bytes, MAX_INPUT_BYTES);
                    assert_eq!(b.remaining_bytes, MAX_INPUT_BYTES as i64 - b.used_bytes as i64, "{case}");
                    assert_eq!(b.status, BudgetStatus::Ok, "{case}");
                }
            }
        }
    }

    #[test]
    fn budget_counts_edge_trimmed_fields_and_whitespace_only_as_absent() {
        let plain = Profile { resume: "R", job_description: "J", focus: "F", extra_instructions: "E", ..Default::default() };
        let padded = Profile {
            resume: "  R\n\n",
            job_description: "\tJ ",
            focus: " F",
            extra_instructions: "E\n",
            ..Default::default()
        };
        let a = local_prompt_budget(plain, AnswerStyle::Brief, "q");
        let b = local_prompt_budget(padded, AnswerStyle::Brief, "q");
        assert_eq!(a, b, "edge whitespace is trimmed by the builder, so it costs nothing");
        assert_eq!(a.profile_bytes, 4);

        let blank = Profile { resume: "   ", focus: "\n", ..Default::default() };
        assert_eq!(
            local_prompt_budget(blank, AnswerStyle::Brief, "q"),
            local_prompt_budget(Profile::default(), AnswerStyle::Brief, "q"),
            "a whitespace-only field adds no header either"
        );
    }

    #[test]
    fn budget_counts_utf8_bytes_not_characters() {
        // 3 emoji (4 bytes each), 2 CJK (3 each), 1 accented letter (2).
        let resume = "😀😀😀日本é";
        let profile = Profile { resume, ..Default::default() };
        let b = local_prompt_budget(profile, AnswerStyle::Balanced, "¿Qué?");
        assert_eq!(b.profile_bytes, 12 + 6 + 2);
        assert_eq!(b.question_bytes, 7, "¿ and é are two bytes each");
        assert_eq!(Some(b.used_bytes), gate_bytes(profile, AnswerStyle::Balanced, "¿Qué?"));
    }

    #[test]
    fn budget_boundaries_match_the_gate_at_limit_minus_one_limit_and_plus_one() {
        for call_type in ALL_CALL_TYPES {
            for style in ALL_STYLES {
                let profile = Profile { call_type, resume: "Resume", ..Default::default() };
                let base = local_prompt_budget(profile, style, "").used_bytes;
                let room = MAX_INPUT_BYTES - base;
                for (n, fits) in [(room - 1, true), (room, true), (room + 1, false)] {
                    let q = ascii(n);
                    let b = local_prompt_budget(profile, style, &q);
                    let case = format!("{call_type:?}/{style:?}/{n}");
                    assert_eq!(b.used_bytes, base + n, "{case}");
                    assert_eq!(b.status == BudgetStatus::Over, !fits, "{case}");
                    assert_eq!(gate_bytes(profile, style, &q).is_some(), fits, "{case}: gate disagrees");
                }
                // Exactly at the limit: fits with nothing to spare.
                let at = local_prompt_budget(profile, style, &ascii(room));
                assert_eq!((at.used_bytes, at.remaining_bytes, at.status), (MAX_INPUT_BYTES, 0, BudgetStatus::Tight));
            }
        }
        // And the refusal past the limit is the gate's own error.
        let req = AnswerRequest::new(build_system_prompt(Profile::default(), AnswerStyle::Brief))
            .with_transcript(ascii(MAX_INPUT_BYTES));
        assert_eq!(request_body(&req).unwrap_err(), crate::llm::local::oversize_error());
    }

    #[test]
    fn an_empty_question_is_over_only_when_not_even_one_byte_fits() {
        // Size a resume so the question-less request lands on limit - 1,
        // limit and limit + 1.
        let one = local_prompt_budget(Profile { resume: "x", ..Default::default() }, AnswerStyle::Brief, "");
        let fill = MAX_INPUT_BYTES - one.fixed_bytes;
        for (resume_len, remaining, status) in [
            (fill - 1, 1, BudgetStatus::Tight),
            (fill, 0, BudgetStatus::Over),
            (fill + 1, -1, BudgetStatus::Over),
        ] {
            let resume = "x".repeat(resume_len);
            let b = local_prompt_budget(Profile { resume: &resume, ..Default::default() }, AnswerStyle::Brief, "");
            assert_eq!((b.remaining_bytes, b.status), (remaining, status), "resume {resume_len}");
        }
    }

    #[test]
    fn the_reserve_separates_ok_from_tight_and_is_not_a_limit() {
        let base = local_prompt_budget(Profile::default(), AnswerStyle::Detailed, "").used_bytes;
        let room = MAX_INPUT_BYTES - base;
        let at_reserve =
            local_prompt_budget(Profile::default(), AnswerStyle::Detailed, &ascii(room - QUESTION_RESERVE_BYTES));
        assert_eq!((at_reserve.remaining_bytes, at_reserve.status), (200, BudgetStatus::Ok));
        let inside =
            local_prompt_budget(Profile::default(), AnswerStyle::Detailed, &ascii(room - QUESTION_RESERVE_BYTES + 1));
        assert_eq!((inside.remaining_bytes, inside.status), (199, BudgetStatus::Tight));
        assert!(gate_bytes(Profile::default(), AnswerStyle::Detailed, &ascii(room - 1)).is_some(), "Tight still answers");
    }

    #[test]
    fn switching_profile_or_style_changes_the_budget_by_exactly_the_text_and_suffix() {
        let short = Profile { resume: "Short", ..Default::default() };
        let long = Profile { resume: "A much longer resume", ..Default::default() };
        let a = local_prompt_budget(short, AnswerStyle::Balanced, "q");
        let b = local_prompt_budget(long, AnswerStyle::Balanced, "q");
        assert_eq!(b.used_bytes - a.used_bytes, "A much longer resume".len() - "Short".len());
        assert_eq!(a.fixed_bytes, b.fixed_bytes);
        let d = local_prompt_budget(short, AnswerStyle::Detailed, "q");
        assert_eq!(d.used_bytes - a.used_bytes, STYLE_DETAILED.len() - STYLE_BALANCED.len());
    }

    #[test]
    fn budget_serializes_the_wire_shape() {
        let b = local_prompt_budget(Profile::default(), AnswerStyle::Brief, "");
        let wire = serde_json::to_value(b).unwrap();
        let keys: Vec<&str> = wire.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["fixedBytes", "limitBytes", "profileBytes", "questionBytes", "remainingBytes", "reserveBytes", "status", "usedBytes"]
        );
        assert_eq!(wire["status"], "ok");
        assert_eq!(serde_json::to_value(BudgetStatus::Tight).unwrap(), "tight");
        assert_eq!(serde_json::to_value(BudgetStatus::Over).unwrap(), "over");
    }
}
