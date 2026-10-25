//! `gridcraft-cli mirai`: asks the Mirai Assistant for suggestions on a range and prints them as
//! JSON. It is a preview: nothing is applied and no file is written.

use std::sync::atomic::AtomicBool;

use gridcraft_assistant::{Client, MAX_INSTRUCTION_BYTES, Provider, Request, Response, model_name};
use serde_json::{Value, json};

use super::{Args, USAGE, exec, open, pretty};

const ASSISTANT_NAME: &str = "Mirai Assistant";

#[derive(Debug, PartialEq, Eq)]
struct Options {
    input: String,
    range: String,
    instruction: String,
    sheet: Option<String>,
    model: Option<String>,
}

fn options(args: &[String]) -> Result<Options, String> {
    let a = Args::parse(args, &["in", "range", "instruction", "sheet", "model"], &[])?;
    a.no_extra(0)?;
    let required = |name: &str| a.opt(name).map(str::to_owned).ok_or_else(|| format!("mirai needs --{name}\n{USAGE}"));
    let instruction = required("instruction")?;
    if instruction.trim().is_empty() {
        return Err("--instruction is empty".into());
    }
    if instruction.len() > MAX_INSTRUCTION_BYTES {
        return Err(format!("--instruction is longer than {MAX_INSTRUCTION_BYTES} bytes"));
    }
    let model = model_name(a.opt("model")).map_err(|e| e.to_string())?;
    let sheet = a.opt("sheet").filter(|s| !s.trim().is_empty()).map(str::to_owned);
    Ok(Options { input: required("in")?, range: required("range")?, instruction, sheet, model })
}

/// Builds the request from the snapshot. Only cell text is sent; the context is left out so the
/// same text isn't sent twice.
fn request_from_context(ctx: &Value, instruction: &str) -> Result<Request, String> {
    let targets = ctx.get("targets").and_then(Value::as_array).ok_or("the context has no targets")?;
    let texts = targets
        .iter()
        .map(|t| t.get("text").and_then(Value::as_str).map(str::to_owned).ok_or_else(|| "a context target has no text".to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    let request = Request { instruction: instruction.to_owned(), targets: texts, context: None };
    request.validate().map_err(|e| e.to_string())?;
    Ok(request)
}

fn output(ctx: Value, response: &Response) -> Value {
    let changes: Vec<Value> = response.suggestions.iter().map(|s| json!({"index": s.index, "value": s.text})).collect();
    json!({
        "assistant": ASSISTANT_NAME,
        "preview": true,
        "applied": false,
        "note": "Preview only: nothing was applied. The context is tied to this open session (document_id and revision) and can't be applied after a restart.",
        "context": ctx,
        "changes": changes,
        "explanation": response.explanation,
    })
}

pub fn run(args: &[String]) -> Result<(), String> {
    let opts = options(args)?;
    let mut s = open(&opts.input)?;
    let mut params = json!({"range": opts.range});
    if let Some(sheet) = &opts.sheet {
        params["sheet"] = json!(sheet);
    }
    let ctx = exec(&mut s, "assistant.context", params)?;
    let request = request_from_context(&ctx, &opts.instruction)?;
    let client = Client::discover().map_err(|e| e.to_string())?.with_model(opts.model.as_deref()).map_err(|e| e.to_string())?;
    let cancel = AtomicBool::new(false);
    let response = client.generate(&request, &cancel).map_err(|e| e.to_string())?;
    outln!("{}", pretty(&output(ctx, &response)));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gridcraft_assistant::Suggestion;

    fn argv(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn valid<'a>() -> Vec<&'a str> {
        vec!["--in", "book.xlsx", "--range", "A1:B2", "--instruction", "Translate to English"]
    }

    #[test]
    fn parses_all_options() {
        let mut v = valid();
        v.extend(["--sheet", "Sales", "--model", "sonnet"]);
        let o = options(&argv(&v)).unwrap();
        assert_eq!(
            o,
            Options {
                input: "book.xlsx".into(),
                range: "A1:B2".into(),
                instruction: "Translate to English".into(),
                sheet: Some("Sales".into()),
                model: Some("sonnet".into()),
            }
        );
    }

    #[test]
    fn blank_sheet_and_model_mean_defaults() {
        let mut v = valid();
        v.extend(["--sheet", "  ", "--model", ""]);
        let o = options(&argv(&v)).unwrap();
        assert_eq!(o.sheet, None);
        assert_eq!(o.model, None);
    }

    #[test]
    fn required_options_are_enforced() {
        for missing in ["--in", "--range", "--instruction"] {
            let mut v = valid();
            let i = v.iter().position(|a| *a == missing).unwrap();
            v.drain(i..i + 2);
            assert!(options(&argv(&v)).is_err(), "{missing}");
        }
    }

    #[test]
    fn strict_parsing_refuses_junk() {
        let mut unknown = valid();
        unknown.extend(["--out", "x.xlsx"]);
        assert!(options(&argv(&unknown)).is_err());
        let mut positional = valid();
        positional.push("extra");
        assert!(options(&argv(&positional)).is_err());
        let mut flag_model = valid();
        flag_model.extend(["--model", "--dangerous"]);
        assert!(options(&argv(&flag_model)).is_err());
    }

    #[test]
    fn instruction_is_bounded_and_not_blank() {
        let mut blank = valid();
        blank[5] = "   ";
        assert!(options(&argv(&blank)).is_err());
        let long = "a".repeat(MAX_INSTRUCTION_BYTES + 1);
        let mut too_long = valid();
        too_long[5] = long.as_str();
        assert!(options(&argv(&too_long)).is_err());
    }

    #[test]
    fn request_carries_target_text_without_context() {
        let ctx = json!({"targets": [{"cell": "A1", "text": "hola"}, {"cell": "B1", "text": ""}]});
        let r = request_from_context(&ctx, "Translate").unwrap();
        assert_eq!(r.instruction, "Translate");
        assert_eq!(r.targets, vec!["hola".to_string(), String::new()]);
        assert_eq!(r.context, None);
    }

    #[test]
    fn malformed_context_is_refused() {
        assert!(request_from_context(&json!({}), "x").is_err());
        assert!(request_from_context(&json!({"targets": [{"cell": "A1"}]}), "x").is_err());
    }

    #[test]
    fn output_is_a_preview_with_changes() {
        let response = Response { suggestions: vec![Suggestion { index: 1, text: "hello".into() }], explanation: "Translated".into() };
        let ctx = json!({"document_id": 7, "revision": 3, "targets": []});
        let out = output(ctx.clone(), &response);
        assert_eq!(out["assistant"], "Mirai Assistant");
        assert_eq!(out["applied"], false);
        assert_eq!(out["preview"], true);
        assert_eq!(out["context"], ctx);
        assert_eq!(out["changes"], json!([{"index": 1, "value": "hello"}]));
        assert_eq!(out["explanation"], "Translated");
    }
}
