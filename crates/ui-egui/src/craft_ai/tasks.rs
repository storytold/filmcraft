//! What Craft AI does inside FilmCraft. Craft AI is one assistant across the ArtCraft family
//! — the model, the runtime and the shared folder belong to the family — but its *work* is the
//! app's own: the system prompt tells the model which app it is inside and what that app does,
//! and every ready-made task is framed for that app's world, so the answer that comes back is
//! about cuts and pacing and captions, not generic advice.
//!
//! The app's own AI — Whisper transcription and the text-to-speech voice — stays beside Craft
//! AI as its specialized helpers for captions and narration; the About tab says so plainly.
//!
//! The labels and hints are catalog keys: the panel shows them translated.

/// The app Craft AI is embedded in, as the shared folder's records name it.
pub const APP: &str = "FilmCraft";

/// One ready-made task of the Assistant tab.
pub struct Task {
    /// The label the picker shows (a catalog key).
    pub label: &'static str,
    /// The input field's hint (a catalog key).
    pub hint: &'static str,
}

/// The tasks, in the picker's order.
pub const TASKS: &[Task] = &[
    Task { label: "Ask anything", hint: "Ask in any language" },
    Task { label: "Cut plan feedback", hint: "The sequence and what it is for" },
    Task { label: "Caption and transcript cleanup", hint: "Paste the transcript or captions" },
    Task { label: "Pacing and montage advice", hint: "The shots, in order, and the mood" },
    Task { label: "Effect or transition choice", hint: "The moment and the feel you want" },
    Task { label: "Name this sequence", hint: "What is in it, for names" },
];

/// Tell the model where it is and how to answer: briefly, practically, in the language the user
/// wrote in, about this app's own world — the words of its interface, the files it opens and
/// writes. The last line is the honesty clause: a local model can be wrong, and it is told to
/// say so rather than invent.
pub fn system_prompt() -> String {
    "You are Craft AI, the assistant built into FilmCraft, a desktop video editor. \
     FilmCraft's world, in its own words: the project and its media browser; the source and \
     program monitors; the timeline and its tracks (video, audio, titles); cuts, trims and \
     rippling; J and L cuts; transitions and effects, keyframes and their curves; Lumetri \
     colour and its scopes; the audio track mixer and clip mixer, ducking and loudness; \
     titles and essential graphics, text-to-speech narration; captions and transcripts, \
     Whisper transcription; markers, in and out points, timecode; export and encoding. \
     Answer in the language the user wrote in, Arabic included. Be brief and practical: name \
     the panel or the tool, give the steps as a short list, say what to watch out for, and \
     when a question is not about FilmCraft or video, answer it anyway, briefly. Never invent \
     menu entries or panel names: if you are not sure FilmCraft has a feature, say so plainly."
        .to_string()
}

/// Frame the user's input for the task they picked.
pub fn user_prompt(task: usize, input: &str) -> String {
    let input = input.trim();
    match TASKS[task.min(TASKS.len() - 1)].label {
        "Cut plan feedback" => format!(
            "Review this cut plan in FilmCraft — what works, what drags, what to try next: {input}"
        ),
        "Caption and transcript cleanup" => format!(
            "Clean up these captions or this transcript for FilmCraft — fix the breaks and the punctuation, keep the speaker's words: {input}"
        ),
        "Pacing and montage advice" => format!(
            "Advise on the pacing and montage of these shots in FilmCraft — order, lengths, where to cut on action: {input}"
        ),
        "Effect or transition choice" => format!(
            "Which effect or transition fits this moment in FilmCraft, and how to set it up — name it and say why: {input}"
        ),
        "Name this sequence" => format!(
            "Suggest short, fitting names for the sequence described below — a handful of options, no explanations: {input}"
        ),
        _ => input.to_string(),
    }
}
