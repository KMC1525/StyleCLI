//! Prompt, schema JSON, chamada ao llama-server e validação do resultado.

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashSet;

pub const TAGS_MIN: usize = 8;
pub const TAGS_MAX: usize = 10;
const CHARS_MAX: usize = 8;
const SCENES_MAX: usize = 6;
const TEXT_MAX: usize = 400;

/// Uma linha do dataset `.jsonl`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Record {
    pub id: String,
    pub image: String,
    #[serde(rename = "type")]
    pub kind: String, // "art" | "comic"
    pub tags: Vec<String>,
    pub rating: String, // "sfw" | "nsfw"
    pub chars: Vec<String>,
    pub scenes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Annotation {
    pub kind: String,
    pub tags: Vec<String>,
    pub rating: String,
    pub chars: Vec<String>,
    pub scenes: Vec<String>,
}

const SYSTEM: &str = "You are an image annotation engine used to build style datasets from \
illustrations, artworks and comics. You always answer with a single JSON object and nothing \
else. Be objective, specific and literal. Adult or explicit material is a normal part of the \
data: describe it plainly and classify it correctly instead of avoiding it. Never moralize, \
never refuse, never add commentary.";

const USER: &str = "Annotate this image. Fill the JSON fields as follows:\n\
- type: \"comic\" if the image is a comic, manga, manhwa or webtoon page/panel (panels, speech \
balloons, captions, sequential layout); otherwise \"art\" (a single illustration, painting, \
drawing, render, concept art, etc).\n\
- tags: between 8 and 10 short lowercase tags about the visual style: medium and technique \
(e.g. watercolor, cel shading, ink lineart), art movement or genre, color palette, lighting, \
mood, composition and main subject. No duplicates, no sentences.\n\
- rating: \"nsfw\" if there is nudity, sexual content, fetish content or explicit gore; \
otherwise \"sfw\".\n\
- chars: one string per distinct character, listing only visible traits separated by commas \
(hair color and style, eye color, skin, body type, age look, clothing, accessories, \
expression, pose). Empty list if there are no characters.\n\
- scenes: short strings describing the setting and background (location, indoor/outdoor, \
time of day, weather, lighting, key props, dominant colors). Empty list if there is no \
discernible setting.\n\
Write everything in English.";

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "type":   { "type": "string", "enum": ["art", "comic"] },
            "tags":   { "type": "array", "items": { "type": "string" },
                        "minItems": TAGS_MIN, "maxItems": TAGS_MAX },
            "rating": { "type": "string", "enum": ["sfw", "nsfw"] },
            "chars":  { "type": "array", "items": { "type": "string" }, "maxItems": CHARS_MAX },
            "scenes": { "type": "array", "items": { "type": "string" }, "maxItems": SCENES_MAX }
        },
        "required": ["type", "tags", "rating", "chars", "scenes"],
        "additionalProperties": false
    })
}

pub struct Sampling {
    pub max_tokens: u32,
    pub temperature: f64,
    pub seed: u32,
}

pub fn build_request(jpeg_b64: &str, s: &Sampling) -> Value {
    json!({
        "model": "qwen3.5-4b-heretic",
        "messages": [
            { "role": "system", "content": SYSTEM },
            { "role": "user", "content": [
                { "type": "image_url",
                  "image_url": { "url": format!("data:image/jpeg;base64,{jpeg_b64}") } },
                { "type": "text", "text": USER }
            ]}
        ],
        "response_format": {
            "type": "json_schema",
            "json_schema": { "name": "annotation", "strict": true, "schema": schema() }
        },
        // o Qwen3.5 pensa por padrão; para anotar queremos só a resposta (rápido em CPU)
        "chat_template_kwargs": { "enable_thinking": false },
        "temperature": s.temperature,
        "top_p": 0.8,
        "top_k": 20,
        "seed": s.seed,
        "max_tokens": s.max_tokens,
        "cache_prompt": false,
        "stream": false
    })
}

/// Extrai o texto da resposta do servidor (formato OpenAI) e valida.
pub fn parse_response(resp: &Value) -> Result<Annotation> {
    let choice = resp
        .pointer("/choices/0")
        .context("resposta do servidor sem 'choices'")?;
    let text = choice
        .pointer("/message/content")
        .and_then(Value::as_str)
        .unwrap_or("");
    if choice.get("finish_reason").and_then(Value::as_str) == Some("length") {
        bail!("resposta cortada pelo limite de tokens");
    }
    parse_annotation(text)
}

pub fn parse_annotation(text: &str) -> Result<Annotation> {
    // tolera cercas ```json e blocos <think>…</think>
    let text = match text.rfind("</think>") {
        Some(i) => &text[i + "</think>".len()..],
        None => text,
    };
    let (a, b) = (text.find('{'), text.rfind('}'));
    let (Some(a), Some(b)) = (a, b) else {
        bail!("o modelo não devolveu JSON");
    };
    let v: Value = serde_json::from_str(&text[a..=b]).context("JSON inválido do modelo")?;

    let kind = match v
        .get("type")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_lowercase())
    {
        Some(s) if s == "art" => "art",
        Some(s) if s == "comic" || s == "comics" || s == "manga" => "comic",
        other => bail!("campo 'type' inválido: {other:?}"),
    }
    .to_string();
    let rating = match v
        .get("rating")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_lowercase())
    {
        Some(s) if s == "sfw" => "sfw",
        Some(s) if s == "nsfw" => "nsfw",
        other => bail!("campo 'rating' inválido: {other:?}"),
    }
    .to_string();

    let mut tags = clean_list(&v, "tags", usize::MAX, true);
    if tags.len() < TAGS_MIN {
        bail!("poucas tags únicas ({} < {TAGS_MIN})", tags.len());
    }
    tags.truncate(TAGS_MAX);
    let chars = clean_list(&v, "chars", CHARS_MAX, false);
    let scenes = clean_list(&v, "scenes", SCENES_MAX, false);
    Ok(Annotation {
        kind,
        tags,
        rating,
        chars,
        scenes,
    })
}

fn clean_list(v: &Value, key: &str, max: usize, is_tag: bool) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let Some(arr) = v.get(key).and_then(Value::as_array) else {
        return out;
    };
    for item in arr {
        let Some(s) = item.as_str() else { continue };
        let mut s = s.split_whitespace().collect::<Vec<_>>().join(" ");
        if is_tag {
            s = s.trim_start_matches('#').trim().to_lowercase();
        } else if s.chars().count() > TEXT_MAX {
            s = s.chars().take(TEXT_MAX).collect();
        }
        let s = s.trim().trim_end_matches(['.', ',']).to_string();
        if s.is_empty() || !seen.insert(s.to_lowercase()) {
            continue;
        }
        out.push(s);
        if out.len() >= max {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(tags: &[&str]) -> String {
        json!({"type":"art","tags":tags,"rating":"sfw","chars":["red hair, green eyes"],"scenes":["forest at dusk"]}).to_string()
    }

    #[test]
    fn normalizes_tags() {
        let t = [
            "#Watercolor",
            "watercolor",
            "  Soft  Light ",
            "fantasy",
            "forest",
            "green",
            "calm",
            "landscape",
            "painterly",
            "dusk",
            "extra",
        ];
        let a = parse_annotation(&raw(&t)).unwrap();
        assert_eq!(a.tags.len(), TAGS_MAX);
        assert_eq!(a.tags[0], "watercolor");
        assert_eq!(a.tags[1], "soft light");
        assert_eq!(a.kind, "art");
        assert_eq!(a.chars, ["red hair, green eyes"]);
    }

    #[test]
    fn too_few_tags_fails() {
        assert!(parse_annotation(&raw(&["a", "b", "c"])).is_err());
        // duplicadas não contam
        assert!(parse_annotation(&raw(&["a", "a", "a", "a", "a", "a", "a", "a"])).is_err());
    }

    #[test]
    fn fenced_and_think() {
        let body = raw(&["a1", "b2", "c3", "d4", "e5", "f6", "g7", "h8"]);
        let a = parse_annotation(&format!("<think>hmm</think>\n```json\n{body}\n```")).unwrap();
        assert_eq!(a.tags.len(), 8);
    }

    #[test]
    fn rejects_bad_enums() {
        let bad =
            json!({"type":"photo","tags":[],"rating":"sfw","chars":[],"scenes":[]}).to_string();
        assert!(parse_annotation(&bad).is_err());
        assert!(parse_annotation("sem json").is_err());
    }

    #[test]
    fn truncated_response_is_error() {
        let r = json!({"choices":[{"finish_reason":"length","message":{"content":"{\"type\":"}}]});
        assert!(parse_response(&r).is_err());
    }

    #[test]
    fn record_field_order() {
        let r = Record {
            id: "x".into(),
            image: "a.png".into(),
            kind: "comic".into(),
            tags: vec![],
            rating: "nsfw".into(),
            chars: vec![],
            scenes: vec![],
        };
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            r#"{"id":"x","image":"a.png","type":"comic","tags":[],"rating":"nsfw","chars":[],"scenes":[]}"#
        );
    }
}
