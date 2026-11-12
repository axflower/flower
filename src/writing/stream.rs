//! Bounded SSE decoding. No provider diagnostics are exposed to the UI.

use super::config::ApiProtocol;
use super::{WritingError, MAX_EVENT_BYTES, MAX_OUTPUT_CHARS, MAX_RESPONSE_BYTES};
use serde_json::Value;

#[derive(Default)]
pub struct SseDecoder {
    received: usize,
    line: Vec<u8>,
    data: String,
    text: String,
    stopped: bool,
    done: bool,
    protocol: ApiProtocol,
    bigmodel: bool,
    item: Option<String>,
    item_index: Option<u64>,
    reasoning_items: Vec<(u64, String)>,
    failure: Option<WritingError>,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::with_protocol(ApiProtocol::ChatCompletions, true)
    }

    pub fn with_protocol(protocol: ApiProtocol, bigmodel: bool) -> Self {
        Self {
            protocol,
            bigmodel,
            ..Self::default()
        }
    }

    /// Return only newly decoded visible text; never include reasoning/errors.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, WritingError> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        let result = self.push_inner(bytes);
        if let Err(error) = result {
            self.failure = Some(error);
        }
        result
    }

    fn push_inner(&mut self, bytes: &[u8]) -> Result<Vec<String>, WritingError> {
        self.received = self
            .received
            .checked_add(bytes.len())
            .filter(|n| *n <= MAX_RESPONSE_BYTES)
            .ok_or(WritingError::TooLarge)?;
        let mut chunks = Vec::new();
        for &byte in bytes {
            if byte == b'\n' {
                if self.line.last() == Some(&b'\r') {
                    self.line.pop();
                }
                let line = String::from_utf8(std::mem::take(&mut self.line))
                    .map_err(|_| WritingError::Protocol)?;
                self.line(&line, &mut chunks)?;
            } else {
                self.line.push(byte);
                if self.line.len() + self.data.len() > MAX_EVENT_BYTES {
                    return Err(WritingError::TooLarge);
                }
            }
        }
        Ok(chunks)
    }

    fn line(&mut self, line: &str, chunks: &mut Vec<String>) -> Result<(), WritingError> {
        if line.is_empty() {
            if self.data.is_empty() {
                return Ok(());
            }
            let data = std::mem::take(&mut self.data);
            return self.event(data.trim_end_matches('\n'), chunks);
        }
        if self.done {
            return if line.starts_with(':') {
                Ok(())
            } else {
                Err(WritingError::Protocol)
            };
        }
        if let Some(value) = line.strip_prefix("data:") {
            let value = value.strip_prefix(' ').unwrap_or(value);
            if self.data.len() + value.len() + 1 > MAX_EVENT_BYTES {
                return Err(WritingError::TooLarge);
            }
            self.data.push_str(value);
            self.data.push('\n');
        }
        Ok(())
    }

    fn event(&mut self, data: &str, chunks: &mut Vec<String>) -> Result<(), WritingError> {
        if self.done {
            return Err(WritingError::Protocol);
        }
        if self.protocol == ApiProtocol::Responses {
            let event: Value = serde_json::from_str(data).map_err(|_| WritingError::Protocol)?;
            return self.response_event(&event, chunks);
        }
        if data == "[DONE]" {
            if !self.stopped {
                return Err(WritingError::Incomplete);
            }
            self.done = true;
            return Ok(());
        }
        let event: Value = serde_json::from_str(data).map_err(|_| WritingError::Protocol)?;
        if event.get("error").is_some() {
            // Codes are categorized locally; never propagate provider messages.
            let code = &event["error"]["code"];
            let code = code
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| code.to_string());
            return Err(if self.bigmodel {
                WritingError::from_provider_code(&code).unwrap_or(WritingError::Service)
            } else {
                WritingError::Service
            });
        }
        let choices = event["choices"].as_array().ok_or(WritingError::Protocol)?;
        if choices.is_empty() && event.get("usage").is_some() {
            return Ok(());
        }
        if choices.len() != 1 || choices[0]["index"].as_u64().unwrap_or(0) != 0 {
            return Err(WritingError::Protocol);
        }
        let choice = &choices[0];
        let delta = &choice["delta"];
        if delta.get("tool_calls").is_some()
            || delta.get("function_call").is_some()
            || delta.get("refusal").is_some_and(|v| !v.is_null())
        {
            return Err(WritingError::Rejected);
        }
        // Compatible providers may stream reasoning separately. Validate its
        // type, but never retain it in visible text or emit it to callers.
        if delta
            .get("reasoning_content")
            .is_some_and(|value| !value.is_null() && !value.is_string())
        {
            return Err(WritingError::Protocol);
        }
        if let Some(content) = delta.get("content").filter(|c| !c.is_null()) {
            let content = content.as_str().ok_or(WritingError::Protocol)?;
            if self.stopped && !content.is_empty() {
                return Err(WritingError::Protocol);
            }
            if content
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\r' && c != '\t')
            {
                return Err(WritingError::Protocol);
            }
            if self.text.chars().count() + content.chars().count() > MAX_OUTPUT_CHARS {
                return Err(WritingError::TooLarge);
            }
            if !content.is_empty() {
                self.text.push_str(content);
                chunks.push(content.into());
            }
        }
        if let Some(reason) = choice.get("finish_reason").filter(|r| !r.is_null()) {
            match reason.as_str() {
                Some("stop") if !self.stopped => self.stopped = true,
                Some("sensitive" | "content_filter") => return Err(WritingError::Rejected),
                Some("length") => return Err(WritingError::Incomplete),
                _ => return Err(WritingError::Protocol),
            }
        }
        Ok(())
    }

    fn response_event(
        &mut self,
        event: &Value,
        chunks: &mut Vec<String>,
    ) -> Result<(), WritingError> {
        let kind = event["type"].as_str().ok_or(WritingError::Protocol)?;
        match kind {
            "response.created" | "response.in_progress" => Ok(()),
            "response.output_item.added" | "response.output_item.done" => {
                let item = &event["item"];
                if item["type"] == "reasoning" {
                    Self::validate_reasoning_item(item, kind.ends_with(".done"))?;
                    return self.reasoning_identity(event, item["id"].as_str());
                }
                if item["type"] != "message" || item["role"] != "assistant" {
                    return Err(WritingError::Rejected);
                }
                self.response_identity(event, item["id"].as_str())?;
                if kind.ends_with(".done") {
                    if item["status"] != "completed" {
                        return Err(WritingError::Incomplete);
                    }
                    let parts = item["content"].as_array().ok_or(WritingError::Protocol)?;
                    if parts.len() != 1 || parts[0]["type"] != "output_text" {
                        return Err(WritingError::Rejected);
                    }
                    if parts[0]["text"].as_str() != Some(self.text.as_str()) {
                        return Err(WritingError::Protocol);
                    }
                }
                if let Some(parts) = item["content"].as_array() {
                    if parts.iter().any(|p| p["type"] != "output_text") {
                        return Err(WritingError::Rejected);
                    }
                }
                Ok(())
            }
            "response.reasoning_summary_part.added"
            | "response.reasoning_summary_part.done"
            | "response.reasoning_summary_text.delta"
            | "response.reasoning_summary_text.done"
            | "response.reasoning_text.delta"
            | "response.reasoning_text.done" => {
                // Retain only identity, never the reasoning text or summary.
                self.reasoning_identity(event, event["item_id"].as_str())?;
                let index = event
                    .get("summary_index")
                    .or_else(|| event.get("content_index"));
                if index.and_then(Value::as_u64).is_none() {
                    return Err(WritingError::Protocol);
                }
                if kind.contains("_part.") {
                    if event["part"]["type"] != "summary_text" || !event["part"]["text"].is_string()
                    {
                        return Err(WritingError::Protocol);
                    }
                } else {
                    let field = if kind.ends_with(".delta") {
                        "delta"
                    } else {
                        "text"
                    };
                    if !event[field].is_string() {
                        return Err(WritingError::Protocol);
                    }
                }
                Ok(())
            }
            "response.content_part.added" | "response.content_part.done" => {
                self.response_identity(event, event["item_id"].as_str())?;
                if event["content_index"] != 0 || event["part"]["type"] != "output_text" {
                    return Err(WritingError::Rejected);
                }
                if kind.ends_with(".done")
                    && event["part"]["text"].as_str() != Some(self.text.as_str())
                {
                    return Err(WritingError::Protocol);
                }
                Ok(())
            }
            "response.output_text.delta" | "response.output_text.done" => {
                self.response_identity(event, event["item_id"].as_str())?;
                if event["content_index"] != 0 {
                    return Err(WritingError::Protocol);
                }
                if kind.ends_with(".done") {
                    return if event["text"].as_str() == Some(self.text.as_str()) {
                        Ok(())
                    } else {
                        Err(WritingError::Protocol)
                    };
                }
                let text = event["delta"].as_str().ok_or(WritingError::Protocol)?;
                if text
                    .chars()
                    .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
                {
                    return Err(WritingError::Protocol);
                }
                if self.text.chars().count() + text.chars().count() > MAX_OUTPUT_CHARS {
                    return Err(WritingError::TooLarge);
                }
                self.text.push_str(text);
                if !text.is_empty() {
                    chunks.push(text.to_owned());
                }
                Ok(())
            }
            "response.completed" => {
                let response = &event["response"];
                if response["status"] != "completed"
                    || response.get("error").is_some_and(|v| !v.is_null())
                    || response
                        .get("incomplete_details")
                        .is_some_and(|v| !v.is_null())
                {
                    return Err(WritingError::Incomplete);
                }
                let output = response["output"]
                    .as_array()
                    .ok_or(WritingError::Protocol)?;
                let mut message = None;
                for (index, item) in output.iter().enumerate() {
                    if item["type"] == "reasoning" {
                        Self::validate_reasoning_item(item, true)?;
                        let identity = serde_json::json!({"output_index": index});
                        self.reasoning_identity(&identity, item["id"].as_str())?;
                    } else if item["type"] == "message" && message.is_none() {
                        message = Some((index, item));
                    } else {
                        return Err(WritingError::Rejected);
                    }
                }
                let (index, item) = message.ok_or(WritingError::Rejected)?;
                if self.item_index != Some(index as u64)
                    || self.reasoning_items.iter().any(|(index, id)| {
                        output.get(*index as usize).map_or(true, |item| {
                            item["type"] != "reasoning" || item["id"].as_str() != Some(id.as_str())
                        })
                    })
                {
                    return Err(WritingError::Protocol);
                }
                if item["type"] != "message"
                    || item["role"] != "assistant"
                    || item["status"] != "completed"
                {
                    return Err(WritingError::Rejected);
                }
                if item["id"].as_str() != self.item.as_deref() {
                    return Err(WritingError::Protocol);
                }
                let parts = item["content"].as_array().ok_or(WritingError::Protocol)?;
                if parts.len() != 1 || parts[0]["type"] != "output_text" {
                    return Err(WritingError::Rejected);
                }
                if parts[0]["text"].as_str() != Some(self.text.as_str()) {
                    return Err(WritingError::Protocol);
                }
                self.stopped = true;
                self.done = true;
                Ok(())
            }
            "response.incomplete" => Err(WritingError::Incomplete),
            "response.failed" | "error" => Err(WritingError::Service),
            // Unknown execution events are not safe to silently ignore.
            _ => Err(WritingError::Rejected),
        }
    }

    fn response_identity(&mut self, event: &Value, id: Option<&str>) -> Result<(), WritingError> {
        let index = event["output_index"]
            .as_u64()
            .ok_or(WritingError::Protocol)?;
        if index >= MAX_RESPONSE_BYTES as u64 {
            return Err(WritingError::Protocol);
        }
        let id = id
            .filter(|id| !id.is_empty())
            .ok_or(WritingError::Protocol)?;
        if self.item.as_deref().is_some_and(|previous| previous != id)
            || self.item_index.is_some_and(|previous| previous != index)
            || self
                .reasoning_items
                .iter()
                .any(|(i, other)| *i == index || other == id)
        {
            return Err(WritingError::Protocol);
        }
        self.item = Some(id.to_owned());
        self.item_index = Some(index);
        Ok(())
    }

    fn reasoning_identity(&mut self, event: &Value, id: Option<&str>) -> Result<(), WritingError> {
        let index = event["output_index"]
            .as_u64()
            .ok_or(WritingError::Protocol)?;
        if index >= MAX_RESPONSE_BYTES as u64 {
            return Err(WritingError::Protocol);
        }
        let id = id
            .filter(|id| !id.is_empty())
            .ok_or(WritingError::Protocol)?;
        if self.item_index == Some(index) || self.item.as_deref() == Some(id) {
            return Err(WritingError::Protocol);
        }
        for (previous, other) in &self.reasoning_items {
            if *previous == index || other == id {
                return if *previous == index && other == id {
                    Ok(())
                } else {
                    Err(WritingError::Protocol)
                };
            }
        }
        self.reasoning_items.push((index, id.to_owned()));
        Ok(())
    }

    fn validate_reasoning_item(item: &Value, completed: bool) -> Result<(), WritingError> {
        if let Some(status) = item.get("status") {
            if status != "completed" && (completed || status != "in_progress") {
                return Err(WritingError::Incomplete);
            }
        }
        if let Some(summary) = item.get("summary") {
            let summary = summary.as_array().ok_or(WritingError::Protocol)?;
            if summary
                .iter()
                .any(|part| part["type"] != "summary_text" || !part["text"].is_string())
            {
                return Err(WritingError::Rejected);
            }
        }
        if let Some(content) = item.get("content") {
            let content = content.as_array().ok_or(WritingError::Protocol)?;
            if content
                .iter()
                .any(|part| part["type"] != "reasoning_text" || !part["text"].is_string())
            {
                return Err(WritingError::Rejected);
            }
        }
        Ok(())
    }

    /// EOF without both a normal stop and a DONE event is not success.
    pub fn finish(self) -> Result<String, WritingError> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        if !self.done || !self.stopped || !self.line.is_empty() || !self.data.is_empty() {
            return Err(WritingError::Incomplete);
        }
        if self.text.trim().is_empty() {
            return Err(WritingError::Incomplete);
        }
        Ok(self.text)
    }

    pub fn is_done(&self) -> bool {
        self.done
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn known_business_codes_are_safe_and_unknown_is_not_retryable() {
        use super::*;
        for (code, expected) in [
            ("1302", WritingError::AccountRateLimited(0)),
            ("1305", WritingError::ServiceBusy(0)),
            ("1308", WritingError::UsageLimit),
            ("1310", WritingError::UsageLimit),
            ("1316", WritingError::UsageLimit),
            ("1317", WritingError::UsageLimit),
            ("1318", WritingError::UsageLimit),
            ("1319", WritingError::UsageLimit),
            ("1320", WritingError::UsageLimit),
            ("1321", WritingError::UsageLimit),
            ("1309", WritingError::WrongKeyType),
            ("1311", WritingError::WrongKeyType),
            ("1314", WritingError::WrongKeyType),
            ("1315", WritingError::WrongKeyType),
            ("1113", WritingError::Quota),
            ("1313", WritingError::Service),
            ("unknown", WritingError::Service),
        ] {
            let mut decoder = SseDecoder::new();
            let event =
                format!("data: {{\"error\":{{\"code\":\"{code}\",\"message\":\"PRIVATE\"}}}}\n\n");
            let error = decoder.push(event.as_bytes()).unwrap_err();
            assert_eq!(error, expected);
            assert!(!error.to_string().contains("PRIVATE"));
        }
    }
    use super::*;
    use serde_json::json;

    fn event(text: &str, finish: Option<&str>) -> String {
        format!(
            "data: {}\r\n\r\n",
            json!({"choices":[{"index":0,"delta":{"content":text},"finish_reason":finish}]})
        )
    }
    fn normal(text: &str) -> String {
        event(text, None) + &event("", Some("stop")) + "data: [DONE]\r\n\r\n"
    }

    #[test]
    fn chat_reasoning_is_invisible_and_still_requires_normal_completion() {
        let reasoning = "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"PRIVATE\"},\"finish_reason\":null}]}\n\n";
        let visible = "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":null,\"content\":\"正文\"},\"finish_reason\":null}]}\n\n";
        let wire = reasoning.to_owned() + visible + &event("", Some("stop")) + "data: [DONE]\n\n";
        let mut decoder = SseDecoder::with_protocol(ApiProtocol::ChatCompletions, false);
        let mut chunks = Vec::new();
        for byte in wire.bytes() {
            chunks.extend(decoder.push(&[byte]).unwrap());
        }
        assert_eq!(chunks, vec!["正文"]);
        assert_eq!(decoder.text, "正文");
        assert_eq!(decoder.finish().unwrap(), "正文");

        let mut decoder = SseDecoder::new();
        assert!(decoder.push(reasoning.as_bytes()).unwrap().is_empty());
        assert_eq!(decoder.finish(), Err(WritingError::Incomplete));
        for invalid in [json!(true), json!(1), json!({}), json!([])] {
            let wire = format!(
                "data: {}\n\n",
                json!({"choices":[{
                "index":0,"delta":{"reasoning_content":invalid},"finish_reason":null}]})
            );
            assert_eq!(
                SseDecoder::new().push(wire.as_bytes()),
                Err(WritingError::Protocol)
            );
        }
    }

    fn response_wire(value: Value) -> String {
        format!("data: {value}\n\n")
    }
    fn response_delta(text: &str) -> Value {
        json!({"type":"response.output_text.delta","item_id":"msg1","output_index":0,"content_index":0,"delta":text})
    }
    fn response_complete(text: &str) -> Value {
        json!({"type":"response.completed","response":{"status":"completed","output":[
            {"id":"msg1","type":"message","role":"assistant","status":"completed",
             "content":[{"type":"output_text","text":text}]}]}})
    }
    fn response_item_done(text: &str, index: u64) -> Value {
        json!({"type":"response.output_item.done","output_index":index,
            "item":response_complete(text)["response"]["output"][0].clone()})
    }
    fn response_part_done(text: &str, index: u64) -> Value {
        json!({"type":"response.content_part.done","output_index":index,
            "item_id":"msg1","content_index":0,"part":{"type":"output_text","text":text}})
    }
    fn response_normal_done(text: &str, index: u64) -> String {
        response_wire(
            json!({"type":"response.output_text.done","output_index":index,
            "item_id":"msg1","content_index":0,"text":text}),
        ) + &response_wire(response_part_done(text, index))
            + &response_wire(response_item_done(text, index))
    }
    fn responses() -> SseDecoder {
        SseDecoder::with_protocol(ApiProtocol::Responses, false)
    }

    #[test]
    fn responses_byte_splits_require_matching_completed_snapshot() {
        let text = "中文🙂\n正文";
        let wire = response_wire(response_delta(text))
            + &response_normal_done(text, 0)
            + &response_wire(response_complete(text));
        for split in 0..=wire.len() {
            let mut decoder = responses();
            let mut chunks = decoder.push(&wire.as_bytes()[..split]).unwrap();
            chunks.extend(decoder.push(&wire.as_bytes()[split..]).unwrap());
            assert_eq!(chunks.concat(), text);
            assert_eq!(decoder.finish().unwrap(), text);
        }
        let mut decoder = responses();
        for byte in wire.bytes() {
            decoder.push(&[byte]).unwrap();
        }
        assert_eq!(decoder.finish().unwrap(), text);
    }

    #[test]
    fn responses_eof_and_text_done_are_incomplete() {
        let mut decoder = responses();
        decoder
            .push(response_wire(response_delta("ok")).as_bytes())
            .unwrap();
        decoder
            .push(
                response_wire(json!({"type":"response.output_text.done",
            "item_id":"msg1","output_index":0,"content_index":0,"text":"ok"}))
                .as_bytes(),
            )
            .unwrap();
        assert_eq!(decoder.finish(), Err(WritingError::Incomplete));
        assert_eq!(responses().finish(), Err(WritingError::Incomplete));
    }

    fn assert_bad_done_cannot_be_overridden(bad_done: Value, expected: WritingError) {
        let prefix = response_wire(response_delta("ok"));
        let bad = response_wire(bad_done);
        let completed = response_wire(response_complete("ok"));
        let mut decoder = responses();
        decoder.push(prefix.as_bytes()).unwrap();
        assert_eq!(decoder.push(bad.as_bytes()), Err(expected));
        assert_eq!(decoder.push(completed.as_bytes()), Err(expected));
        assert_eq!(decoder.finish(), Err(expected));
        let mut decoder = responses();
        assert_eq!(
            decoder.push((prefix + &bad + &completed).as_bytes()),
            Err(expected)
        );
        assert_eq!(decoder.finish(), Err(expected));
    }

    #[test]
    fn responses_incomplete_item_done_cannot_be_overridden_by_completed() {
        let mut done = response_item_done("ok", 0);
        done["item"]["status"] = json!("incomplete");
        assert_bad_done_cannot_be_overridden(done, WritingError::Incomplete);
    }

    #[test]
    fn responses_mismatched_item_done_cannot_be_overridden_by_completed() {
        assert_bad_done_cannot_be_overridden(
            response_item_done("different", 0),
            WritingError::Protocol,
        );
    }

    #[test]
    fn responses_mismatched_part_done_cannot_be_overridden_by_completed() {
        assert_bad_done_cannot_be_overridden(
            response_part_done("different", 0),
            WritingError::Protocol,
        );
    }

    #[test]
    fn responses_reject_failed_refused_tools_unknown_and_incomplete() {
        for kind in [
            "response.failed",
            "response.incomplete",
            "error",
            "response.refusal.delta",
            "response.function_call_arguments.delta",
            "response.web_search_call.in_progress",
            "response.unknown",
            "response.reasoning_text.delta",
        ] {
            assert!(responses()
                .push(response_wire(json!({"type":kind,"delta":"private"})).as_bytes())
                .is_err());
        }
        for kind in ["function_call", "refusal"] {
            assert!(responses()
                .push(
                    response_wire(json!({"type":"response.output_item.added",
                "output_index":0,"item":{"id":"msg1","type":kind}}))
                    .as_bytes()
                )
                .is_err());
        }
    }

    #[test]
    fn responses_reject_snapshot_mismatch_multiple_items_and_bad_identity() {
        for variant in 0..7 {
            let mut decoder = responses();
            decoder
                .push(response_wire(response_delta("ok")).as_bytes())
                .unwrap();
            let mut completed = response_complete("ok");
            match variant {
                0 => completed["response"]["output"][0]["content"][0]["text"] = json!("different"),
                1 => completed["response"]["output"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"type":"message"})),
                2 => completed["response"]["output"][0]["id"] = json!("another"),
                3 => completed["response"]["status"] = json!("incomplete"),
                4 => completed["response"]["output"][0]["content"][0]["type"] = json!("refusal"),
                5 => completed["response"]["error"] = json!({"message":"private"}),
                _ => completed["response"]["output"][0]["status"] = json!("in_progress"),
            }
            assert!(decoder.push(response_wire(completed).as_bytes()).is_err());
        }
        for field in ["output_index", "content_index", "item_id"] {
            let mut decoder = responses();
            decoder
                .push(response_wire(response_delta("ok")).as_bytes())
                .unwrap();
            let mut delta = response_delta("ok");
            delta[field] = if field == "item_id" {
                Value::Null
            } else {
                json!(1)
            };
            assert!(decoder.push(response_wire(delta).as_bytes()).is_err());
        }
    }

    fn reasoning_item() -> Value {
        json!({"id":"rs1","type":"reasoning","status":"completed",
            "summary":[{"type":"summary_text","text":"PRIVATE-REASONING"}]})
    }

    fn reasoning_then_text() -> (String, Value) {
        let mut wire = response_wire(json!({"type":"response.output_item.added",
            "output_index":0,"item":reasoning_item()}));
        for kind in [
            "response.reasoning_summary_part.added",
            "response.reasoning_summary_part.done",
            "response.reasoning_summary_text.delta",
            "response.reasoning_summary_text.done",
            "response.reasoning_text.delta",
            "response.reasoning_text.done",
        ] {
            wire += &response_wire(json!({"type":kind,"item_id":"rs1","output_index":0,
                "summary_index":0,"content_index":0,"delta":"PRIVATE-REASONING",
                "text":"PRIVATE-REASONING","part":{"type":"summary_text","text":"PRIVATE-REASONING"}}));
        }
        wire += &response_wire(json!({"type":"response.output_item.done",
            "output_index":0,"item":reasoning_item()}));
        let mut delta = response_delta("正文🙂");
        delta["output_index"] = json!(1);
        wire += &response_wire(delta);
        wire += &response_normal_done("正文🙂", 1);
        let mut completed = response_complete("正文🙂");
        completed["response"]["output"]
            .as_array_mut()
            .unwrap()
            .insert(0, reasoning_item());
        (wire, completed)
    }

    #[test]
    fn responses_reasoning_zero_text_one_never_exposes_reasoning() {
        let (prefix, completed) = reasoning_then_text();
        let wire = prefix + &response_wire(completed);
        let mut decoder = responses();
        let mut chunks = Vec::new();
        for byte in wire.bytes() {
            chunks.extend(decoder.push(&[byte]).unwrap());
        }
        assert_eq!(chunks.concat(), "正文🙂");
        assert_eq!(decoder.text, "正文🙂");
        assert_eq!(decoder.finish().unwrap(), "正文🙂");
    }

    #[test]
    fn responses_reasoning_does_not_allow_tools_or_wrong_message_index() {
        for kind in ["function_call", "web_search_call", "refusal"] {
            let (prefix, mut completed) = reasoning_then_text();
            let mut decoder = responses();
            decoder.push(prefix.as_bytes()).unwrap();
            completed["response"]["output"]
                .as_array_mut()
                .unwrap()
                .push(json!({"type":kind}));
            assert!(decoder.push(response_wire(completed).as_bytes()).is_err());
            let (prefix, _) = reasoning_then_text();
            let mut decoder = responses();
            decoder.push(prefix.as_bytes()).unwrap();
            assert!(decoder
                .push(
                    response_wire(json!({"type":"response.output_item.added",
                "output_index":2,"item":{"type":kind,"id":"tool"}}))
                    .as_bytes()
                )
                .is_err());
        }
        for variant in 0..4 {
            let (prefix, mut completed) = reasoning_then_text();
            let mut decoder = responses();
            decoder.push(prefix.as_bytes()).unwrap();
            if variant == 0 {
                completed["response"]["output"]
                    .as_array_mut()
                    .unwrap()
                    .swap(0, 1);
                assert!(decoder.push(response_wire(completed).as_bytes()).is_err());
            } else {
                let mut delta = response_delta("wrong");
                delta["output_index"] = json!(1);
                match variant {
                    1 => delta["output_index"] = json!(0),
                    2 => delta["item_id"] = json!("different"),
                    _ => delta["content_index"] = json!(1),
                }
                assert!(decoder.push(response_wire(delta).as_bytes()).is_err());
            }
        }
    }

    #[test]
    fn responses_share_limits_and_generic_chat_never_uses_bigmodel_codes() {
        assert_eq!(
            responses().push(response_wire(response_delta(&"花".repeat(801))).as_bytes()),
            Err(WritingError::TooLarge)
        );
        assert_eq!(
            responses().push(response_wire(response_delta("\0")).as_bytes()),
            Err(WritingError::Protocol)
        );
        assert_eq!(
            responses().push(&vec![b'x'; MAX_EVENT_BYTES + 1]),
            Err(WritingError::TooLarge)
        );
        assert_eq!(
            responses().push(&vec![b'x'; MAX_RESPONSE_BYTES + 1]),
            Err(WritingError::TooLarge)
        );
        let mut chat = SseDecoder::with_protocol(ApiProtocol::ChatCompletions, false);
        assert_eq!(
            chat.push(b"data: {\"error\":{\"code\":\"1302\"}}\n\n"),
            Err(WritingError::Service)
        );
        let mut chat = SseDecoder::with_protocol(ApiProtocol::ChatCompletions, false);
        chat.push(normal("ok").as_bytes()).unwrap();
        assert_eq!(chat.finish().unwrap(), "ok");
    }

    #[test]
    fn every_byte_split_preserves_utf8_and_crlf() {
        let wire = normal("原文 Flower0.5.0🙂\n继续");
        for split in 0..=wire.len() {
            let mut decoder = SseDecoder::new();
            let mut chunks = decoder.push(&wire.as_bytes()[..split]).unwrap();
            chunks.extend(decoder.push(&wire.as_bytes()[split..]).unwrap());
            assert_eq!(chunks.concat(), "原文 Flower0.5.0🙂\n继续");
            assert_eq!(decoder.finish().unwrap(), chunks.concat());
        }
        let mut decoder = SseDecoder::new();
        for byte in wire.bytes() {
            decoder.push(&[byte]).unwrap();
        }
        assert!(decoder.finish().is_ok());
    }

    #[test]
    fn accepts_comments_and_multiline_data() {
        let mut decoder = SseDecoder::new();
        decoder.push(b": heartbeat\n\ndata: {\"choices\":\ndata: [{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n").unwrap();
        assert_eq!(decoder.finish().unwrap(), "ok");
    }

    #[test]
    fn fails_closed_on_empty_truncated_and_non_normal_endings() {
        assert_eq!(SseDecoder::new().finish(), Err(WritingError::Incomplete));
        let mut decoder = SseDecoder::new();
        decoder.push(event("partial", None).as_bytes()).unwrap();
        assert_eq!(decoder.finish(), Err(WritingError::Incomplete));
        let mut decoder = SseDecoder::new();
        assert_eq!(
            decoder.push(event("partial", Some("length")).as_bytes()),
            Err(WritingError::Incomplete)
        );
        let mut decoder = SseDecoder::new();
        assert_eq!(
            decoder.push(b"data: [DONE]\n\n"),
            Err(WritingError::Incomplete)
        );
        let mut decoder = SseDecoder::new();
        decoder.push(normal(" ").as_bytes()).unwrap();
        assert_eq!(decoder.finish(), Err(WritingError::Incomplete));
    }

    #[test]
    fn rejects_unbounded_data_invalid_utf8_and_invalid_reasoning_type() {
        let mut decoder = SseDecoder::new();
        assert_eq!(
            decoder.push(&vec![b'x'; MAX_EVENT_BYTES + 1]),
            Err(WritingError::TooLarge)
        );
        let mut decoder = SseDecoder::new();
        assert_eq!(decoder.push(b"data: \xff\n\n"), Err(WritingError::Protocol));
        let mut decoder = SseDecoder::new();
        assert_eq!(
            decoder.push(event(&"🙂".repeat(801), None).as_bytes()),
            Err(WritingError::TooLarge)
        );
        let mut decoder = SseDecoder::new();
        assert_eq!(
            decoder.push(b"data: {\"choices\":[{\"delta\":{\"reasoning_content\":{}}}]}\n\n"),
            Err(WritingError::Protocol)
        );
    }

    #[test]
    fn protocol_errors_never_return_provider_diagnostics() {
        let mut decoder = SseDecoder::new();
        let error = decoder
            .push(b"data: {\"error\":{\"code\":\"1113\",\"message\":\"private-key-and-text\"}}\n\n")
            .unwrap_err();
        assert_eq!(error, WritingError::Quota);
        assert!(!error.to_string().contains("private"));
        let mut decoder = SseDecoder::new();
        decoder.push(normal("ok").as_bytes()).unwrap();
        assert_eq!(
            decoder.push(event("late", None).as_bytes()),
            Err(WritingError::Protocol)
        );
    }
}
