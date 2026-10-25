//! AI (LLM) final-stage transcript refinement and semantic caption segmentation.
//!
//! After Whisper produces word-level timestamps (`0..N`), [`refine_transcript`] sends the indexed
//! words to an LLM (Google Gemini, OpenAI, Anthropic Claude, or OpenRouter) to:
//! 1. Fix phonetic/spelling errors and enforce client terminology (e.g. Abravidro glossary) while
//!    keeping each word's exact audio timestamp (`start`..`end`).
//! 2. Group the words into semantically complete caption blocks (`start_word`..`end_word`) with at
//!    most `max_lines` lines and at most `max_chars` characters per line, never splitting closely
//!    bound phrases (such as noun + adjective like *"jornada semanal"*) or leaving hanging
//!    connectors (articles, prepositions, conjunctions) at the end of a line or block.

use filmcraft_project::Transcript;
use serde::{Deserialize, Serialize};
#[cfg(feature = "download")]
use serde_json::{Value, json};

use crate::SpeechError;

/// Default subtitling & glossary rules sent to the LLM when the user leaves the custom rules
/// field at its default.
pub const DEFAULT_AI_RULES: &str = "\
1. Máximo estrito de 30 caracteres por linha (contando espaços e pontuação) e no máximo 2 linhas por bloco.
2. Sentido semântico completo em cada bloco e linha: NUNCA separe substantivo + adjetivo (ex.: mantenha 'jornada semanal' junto), nem verbo auxiliar + verbo principal, nem locuções curtas.
3. NUNCA termine uma linha ou um bloco com artigos (o, a, os, as, um, uma), preposições (de, da, do, dos, das, em, na, no, por, para, com, sem), conjunções (e, ou, mas, que, se) ou pronomes pendurados.
4. Proibido linhas viúvas/órfãs (nunca deixe 1 palavra isolada sozinha em uma linha).
5. Glossário e correção fonética obrigatória: 'ArtCraft', 'FilmCraft', 'PhotoCraft', 'VectorCraft', 'PdfCraft', 'Timeline', 'Playhead', 'Keyframe', 'B-Roll', 'Color Grading', 'Motion Graphics'.";

/// A single AI-segmented caption block referencing word indices in the [`Transcript`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiCaptionBlock {
    pub start_word: usize,
    pub end_word: usize,
    pub lines: Vec<String>,
}

/// Output of [`refine_transcript`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiRefinementResult {
    pub captions: Vec<AiCaptionBlock>,
    pub corrected_words: usize,
    pub provider_used: String,
}

#[derive(Deserialize)]
struct LlmCorrection {
    index: usize,
    text: String,
}

#[derive(Deserialize)]
struct LlmCaption {
    start_word: usize,
    end_word: usize,
    lines: Vec<String>,
}

#[derive(Deserialize)]
struct LlmResponsePayload {
    #[serde(default)]
    corrections: Vec<LlmCorrection>,
    #[serde(default)]
    captions: Vec<LlmCaption>,
}

/// Detect the provider (`"gemini"`, `"openai"`, `"anthropic"`, `"openrouter"`) from the choice or
/// API key prefix.
pub fn resolve_provider(provider: &str, api_key: &str) -> &'static str {
    let p = provider.trim().to_ascii_lowercase();
    match p.as_str() {
        "gemini" | "google" | "vertex" => "gemini",
        "openai" => "openai",
        "anthropic" | "claude" => "anthropic",
        "openrouter" => "openrouter",
        "groq" => "groq",
        _ => {
            let k = api_key.trim();
            if k.starts_with("AIza") || k.starts_with("AQ") {
                "gemini"
            } else if k.starts_with("sk-ant-") {
                "anthropic"
            } else if k.starts_with("sk-or-") {
                "openrouter"
            } else if k.starts_with("gsk_") {
                "groq"
            } else {
                "openai"
            }
        }
    }
}

/// Refine the words in `t` and compute semantically balanced caption blocks using the configured
/// LLM provider.
pub fn refine_transcript(
    t: &mut Transcript,
    provider: &str,
    api_key: &str,
    custom_rules: &str,
    max_chars: usize,
    max_lines: usize,
) -> Result<AiRefinementResult, SpeechError> {
    let key = api_key.trim();
    if key.is_empty() {
        return Err(SpeechError::Model("AI API token is empty".into()));
    }
    if t.words.is_empty() {
        return Ok(AiRefinementResult::default());
    }
    let resolved = resolve_provider(provider, key);
    let rules = if custom_rules.trim().is_empty() { DEFAULT_AI_RULES } else { custom_rules.trim() };
    let max_chars = max_chars.max(15);
    let max_lines = max_lines.clamp(1, 3);

    // Process in chunks of up to 220 words so long sequences never exceed output token limits.
    let chunk_size = 220usize;
    let mut total_corrected = 0usize;
    let mut all_captions = Vec::new();

    let total_words = t.words.len();
    log::info!("[AI] Starting transcript refinement: {total_words} word(s), provider={resolved}, max_chars={max_chars}, max_lines={max_lines}");
    let mut start_idx = 0usize;
    while start_idx < total_words {
        let mut end_idx = (start_idx + chunk_size).min(total_words);
        // Try to end the chunk at a sentence boundary (., !, ?) if possible.
        if end_idx < total_words {
            for probe in (start_idx + 120..end_idx).rev() {
                if let Some(w) = t.words.get(probe)
                    && w.text.ends_with(['.', '!', '?'])
                {
                    end_idx = probe + 1;
                    break;
                }
            }
        }

        let mut indexed_list = String::new();
        for (i, w) in t.words[start_idx..end_idx].iter().enumerate() {
            let global_i = start_idx + i;
            if !indexed_list.is_empty() {
                indexed_list.push(' ');
            }
            indexed_list.push_str(&format!("[{global_i}]{}", w.text));
        }

        let system_prompt = format!(
            "Você é um editor e revisor especialista em legendagem televisiva e jornalística.\n\
             Sua tarefa é receber uma sequência de palavras transcritas pelo Whisper (numeradas como [índice]palavra, do índice {start_idx} até {last_idx}) e devolver APENAS um objeto JSON válido (sem markdown extra) com duas chaves:\n\
             1. \"corrections\": lista de objetos {{\"index\": <numero>, \"text\": \"<palavra corrigida com pontuação>\"}} APENAS para as palavras que precisam de correção ortográfica, fonética, hifenização ou pontuação. Mantenha correspondência 1-para-1 com o índice.\n\
             2. \"captions\": lista completa de blocos de legenda cobrindo TODOS os índices de {start_idx} a {last_idx} em ordem sequencial, sem pular palavras. Cada item deve ser {{\"start_word\": <indice_inicial>, \"end_word\": <indice_final_inclusivo>, \"lines\": [\"linha 1\", \"linha 2 opcional\"]}}.\n\n\
             REGRAS OBRIGATÓRIAS DE LEGENDAGEM:\n\
             - Limite máximo: {max_chars} caracteres por linha (contando espaços e pontuação) e no máximo {max_lines} linhas por bloco.\n\
             - As linhas de cada bloco devem conter exatamente as palavras (já corrigidas) de start_word até end_word.\n\
             {rules}",
            last_idx = end_idx.saturating_sub(1),
        );

        let user_prompt = format!(
            "Palavras transcritas ({start_idx}..={last_idx}):\n{indexed_list}\n\n\
             Retorne exclusivamente o JSON no formato:\n\
             {{\"corrections\": [{{\"index\": 0, \"text\": \"...\"}}], \"captions\": [{{\"start_word\": {start_idx}, \"end_word\": ..., \"lines\": [\"...\"]}}]}}",
            last_idx = end_idx.saturating_sub(1),
        );

        log::info!("[AI] Requesting chunk {start_idx}..{end_idx} ({} words) from {resolved}...", end_idx - start_idx);
        let raw_json = call_llm_api(resolved, key, &system_prompt, &user_prompt).map_err(|e| {
            log::error!("[AI] API call failed on chunk {start_idx}..{end_idx}: {e}");
            e
        })?;
        let clean_json = extract_json_object(&raw_json);
        let parsed: LlmResponsePayload = serde_json::from_str(clean_json).map_err(|e| {
            log::error!("[AI] JSON parse error on chunk {start_idx}..{end_idx}: {e} | payload: {clean_json}");
            SpeechError::Model(format!("AI JSON parse error: {e}"))
        })?;

        for c in parsed.corrections {
            let trimmed = c.text.trim();
            if !trimmed.is_empty()
                && let Some(w) = t.words.get_mut(c.index)
                && w.text != trimmed
            {
                w.text = trimmed.to_string();
                total_corrected += 1;
            }
        }

        for cap in parsed.captions {
            if cap.start_word < total_words && cap.end_word < total_words && cap.start_word <= cap.end_word {
                let clean_lines: Vec<String> = cap.lines.into_iter().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
                if !clean_lines.is_empty() {
                    all_captions.push(AiCaptionBlock { start_word: cap.start_word, end_word: cap.end_word, lines: clean_lines });
                }
            }
        }

        start_idx = end_idx;
    }

    log::info!("[AI] Refinement complete ({resolved}): {total_corrected} word(s) corrected, {} caption block(s) created", all_captions.len());
    Ok(AiRefinementResult { captions: all_captions, corrected_words: total_corrected, provider_used: resolved.to_string() })
}

fn extract_json_object(s: &str) -> &str {
    let trimmed = s.trim();
    if let Some(start) = trimmed.find('{')
        && let Some(end) = trimmed.rfind('}')
        && start <= end
    {
        return &trimmed[start..=end];
    }
    trimmed
}

#[cfg(feature = "download")]
fn extract_api_error(status: u16, body: &str) -> String {
    if let Ok(val) = serde_json::from_str::<Value>(body) {
        if let Some(msg) = val.pointer("/error/message").and_then(Value::as_str) {
            return format!("HTTP {status}: {msg}");
        }
        if let Some(msg) = val.pointer("/0/error/message").and_then(Value::as_str) {
            return format!("HTTP {status}: {msg}");
        }
    }
    let snippet: String = body.chars().take(240).collect();
    format!("HTTP {status}: {snippet}")
}

#[cfg(feature = "download")]
fn call_llm_api(provider: &str, api_key: &str, system_prompt: &str, user_prompt: &str) -> Result<String, SpeechError> {
    let agent = crate::models::llm_agent();
    match provider {
        "gemini" => {
            let endpoints: &[(&str, bool)] = &[
                ("https://generativelanguage.googleapis.com/v1beta/models/gemini-flash-latest:generateContent", true),
                ("https://generativelanguage.googleapis.com/v1beta/models/gemini-3.8-flash:generateContent", true),
                ("https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent", false),
                ("https://aiplatform.googleapis.com/v1/publishers/google/models/gemini-2.5-flash:generateContent", false),
            ];
            let mut last_err = String::from("no Gemini endpoint succeeded");
            for &(base_url, no_thinking) in endpoints {
                let gen_cfg = if no_thinking {
                    json!({
                        "temperature": 0.1,
                        "responseMimeType": "application/json",
                        "thinkingConfig": {"thinkingBudget": 0}
                    })
                } else {
                    json!({
                        "temperature": 0.1,
                        "responseMimeType": "application/json"
                    })
                };
                let body = json!({
                    "systemInstruction": {
                        "parts": [{"text": system_prompt}]
                    },
                    "contents": [{
                        "role": "user",
                        "parts": [{"text": user_prompt}]
                    }],
                    "generationConfig": gen_cfg
                });
                let url = format!("{base_url}?key={api_key}");
                let send_res = agent.post(&url).header("x-goog-api-key", api_key).header("Content-Type", "application/json").send(&body.to_string());
                let mut resp = match send_res {
                    Ok(r) => r,
                    Err(e) => {
                        last_err = format!("Gemini network error ({base_url}): {e}");
                        log::warn!("[AI] {last_err}");
                        continue;
                    }
                };
                let status = resp.status().as_u16();
                let text = resp.body_mut().read_to_string().map_err(|e| SpeechError::Download(format!("Gemini response read error: {e}")))?;
                if !(200..300).contains(&status) {
                    last_err = extract_api_error(status, &text);
                    log::warn!("[AI] Gemini endpoint {base_url} -> {last_err}");
                    continue;
                }
                let val: Value = serde_json::from_str(&text).map_err(|e| SpeechError::Download(format!("Gemini JSON error: {e}")))?;
                if let Some(parts) = val.pointer("/candidates/0/content/parts").and_then(Value::as_array) {
                    // Pick the last part that has `text` and is not marked `thought: true`.
                    for part in parts.iter().rev() {
                        let is_thought = part.get("thought").and_then(Value::as_bool).unwrap_or(false);
                        if !is_thought && let Some(t) = part.get("text").and_then(Value::as_str) {
                            return Ok(t.to_string());
                        }
                    }
                }
                last_err = format!("Unexpected Gemini response structure: {val}");
                log::warn!("[AI] {last_err}");
            }
            Err(SpeechError::Download(format!("Gemini API error ({last_err})")))
        }
        "anthropic" => {
            let url = "https://api.anthropic.com/v1/messages";
            let body = json!({
                "model": "claude-sonnet-4-6",
                "max_tokens": 4096,
                "temperature": 0.1,
                "system": system_prompt,
                "messages": [
                    {"role": "user", "content": user_prompt}
                ]
            });
            let mut resp = agent
                .post(url)
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01")
                .header("Content-Type", "application/json")
                .send(&body.to_string())
                .map_err(|e| SpeechError::Download(format!("Anthropic API error: {e}")))?;
            let status = resp.status().as_u16();
            let text = resp.body_mut().read_to_string().map_err(|e| SpeechError::Download(format!("Anthropic response error: {e}")))?;
            if !(200..300).contains(&status) {
                return Err(SpeechError::Download(format!("Anthropic {}", extract_api_error(status, &text))));
            }
            let val: Value = serde_json::from_str(&text).map_err(|e| SpeechError::Download(format!("Anthropic JSON error: {e}")))?;
            val.pointer("/content/0/text")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| SpeechError::Model(format!("Unexpected Anthropic response: {val}")))
        }
        _ => {
            // OpenAI, Groq or OpenRouter
            let (url, model) = if provider == "openrouter" {
                ("https://openrouter.ai/api/v1/chat/completions", "openai/gpt-4o-mini")
            } else if provider == "groq" {
                ("https://api.groq.com/openai/v1/chat/completions", "llama-3.3-70b-versatile")
            } else {
                ("https://api.openai.com/v1/chat/completions", "gpt-4o-mini")
            };
            let body = json!({
                "model": model,
                "temperature": 0.1,
                "response_format": {"type": "json_object"},
                "messages": [
                    {"role": "system", "content": system_prompt},
                    {"role": "user", "content": user_prompt}
                ]
            });
            let mut resp = agent
                .post(url)
                .header("Authorization", &format!("Bearer {api_key}"))
                .header("Content-Type", "application/json")
                .send(&body.to_string())
                .map_err(|e| SpeechError::Download(format!("{provider} API error: {e}")))?;
            let status = resp.status().as_u16();
            let text = resp.body_mut().read_to_string().map_err(|e| SpeechError::Download(format!("{provider} response error: {e}")))?;
            if !(200..300).contains(&status) {
                return Err(SpeechError::Download(format!("{provider} {}", extract_api_error(status, &text))));
            }
            let val: Value = serde_json::from_str(&text).map_err(|e| SpeechError::Download(format!("{provider} JSON error: {e}")))?;
            val.pointer("/choices/0/message/content")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| SpeechError::Model(format!("Unexpected {provider} response: {val}")))
        }
    }
}

#[cfg(not(feature = "download"))]
fn call_llm_api(_: &str, _: &str, _: &str, _: &str) -> Result<String, SpeechError> {
    Err(SpeechError::Unavailable("built without the `download` (HTTPS) feature".into()))
}
