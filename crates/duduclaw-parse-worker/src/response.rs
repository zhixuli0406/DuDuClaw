use duduclaw_isolation::worker::*;

/// JSON escaping can expand valid bounded text sixfold. Cap serialization itself
/// and replace oversize/invalid output with a small correlated failure.
pub fn encode(response: &ParseResponse, request: &ParseRequest) -> Result<Vec<u8>, ParseError> {
    struct Limited(Vec<u8>);
    impl std::io::Write for Limited {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAX_WORKER_FRAME_BYTES.saturating_sub(self.0.len()) {
                return Err(std::io::ErrorKind::InvalidData.into());
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    let mut output = Limited(Vec::new());
    let failure = if response.validate_for(request).is_err() { Some(ParseError::InvalidContent) }
        else if serde_json::to_writer(&mut output, response).is_err() { Some(ParseError::LimitExceeded) }
        else { None };
    if let Some(code) = failure {
        let failed = ParseResponse { version: WORKER_PROTOCOL_VERSION, request_id: request.request_id,
            result: ParseResult::Failed { code } };
        serde_json::to_vec(&failed).map_err(|_| ParseError::InvalidContent)
    } else { Ok(output.0) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_isolation::bounded::{List, Text};
    #[test]
    fn escaped_text_over_frame_limit_becomes_bounded_error() {
        let request = crate::client::request(ParseOperation::ExtractDocument, ContentFormat::Text, 0);
        let response = ParseResponse { version: WORKER_PROTOCOL_VERSION, request_id: request.request_id,
            result: ParseResult::Complete { items: List::new((0..16).map(|_| ParsedItem::Text {
                input_slot: 0, page: None, text: Text::new("\u{1}".repeat(8192)).unwrap(),
            }).collect()).unwrap() } };
        assert!(response.validate_for(&request).is_ok());
        let encoded = encode(&response, &request).unwrap();
        assert!(encoded.len() < 256);
        assert_eq!(ParseResponse::decode_payload(&encoded, &request).unwrap().result,
            ParseResult::Failed { code: ParseError::LimitExceeded });
    }
    #[test]
    fn invalid_slot_never_crosses_boundary() {
        let request = crate::client::request(ParseOperation::InspectImage, ContentFormat::Image, 0);
        let response = ParseResponse { version: WORKER_PROTOCOL_VERSION, request_id: request.request_id,
            result: ParseResult::Complete { items: List::new(vec![ParsedItem::Image {
                input_slot: 2, width: 1, height: 1,
            }]).unwrap() } };
        assert_eq!(ParseResponse::decode_payload(&encode(&response, &request).unwrap(), &request).unwrap().result,
            ParseResult::Failed { code: ParseError::InvalidContent });
    }
}
