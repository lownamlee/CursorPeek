//! Bounded entry points shared by coverage-guided fuzzers and deterministic corpus replay.

use std::io::Cursor;

use crate::{
    layout::{
        BGRA_BYTES_PER_PIXEL, MAX_PREVIEW_IMAGE_HEIGHT, MAX_PREVIEW_IMAGE_WIDTH,
        checked_bgra_layout, fitted_preview_dimensions,
    },
    payload::{decode_result, encode_result},
    protocol::{read_message, write_message},
    sniff::{classify_text_prefix, sniff_image_format},
    svg,
};

pub fn exercise_protocol(data: &[u8]) {
    let mut input = Cursor::new(data);

    loop {
        let message = match read_message(&mut input) {
            Ok(Some(message)) => message,
            Ok(None) | Err(_) => return,
        };

        let mut encoded = Vec::new();
        write_message(&mut encoded, message.clone())
            .expect("every accepted protocol message must be encodable");
        let mut round_trip = Cursor::new(encoded);
        assert_eq!(
            read_message(&mut round_trip)
                .expect("an encoded message must be readable")
                .as_ref(),
            Some(&message)
        );
        assert!(
            read_message(&mut round_trip)
                .expect("the encoded message must end cleanly")
                .is_none()
        );
    }
}

pub fn exercise_payload(data: &[u8]) {
    let Ok(result) = decode_result(data) else {
        return;
    };

    let encoded = encode_result(&result).expect("every accepted payload must be encodable");
    assert_eq!(
        decode_result(&encoded).as_ref(),
        Ok(&result),
        "an accepted payload must survive canonical round-trip encoding"
    );
}

pub fn exercise_content_sniff(data: &[u8]) {
    let (prefix_truncated, content) = data
        .split_first()
        .map_or((false, &[][..]), |(flags, content)| {
            (flags & 1 != 0, content)
        });

    let text = classify_text_prefix(content, prefix_truncated);
    assert_eq!(
        classify_text_prefix(content, prefix_truncated),
        text,
        "text classification must be deterministic"
    );

    let image_prefix = &content[..content.len().min(16)];
    let image = sniff_image_format(image_prefix);
    assert_eq!(
        sniff_image_format(image_prefix),
        image,
        "image classification must be deterministic"
    );
}

pub fn exercise_layout(data: &[u8]) {
    let values = [
        little_endian_u32(data, 0),
        little_endian_u32(data, 4),
        little_endian_u32(data, 8),
        little_endian_u32(data, 12),
    ];

    check_fitted_layout(values[0], values[1]);
    check_fitted_layout(values[2], values[3]);

    for (width, height) in [(values[0], values[1]), (values[2], values[3])] {
        if let Ok((stride, length)) = checked_bgra_layout(width, height) {
            assert!(width <= MAX_PREVIEW_IMAGE_WIDTH);
            assert!(height <= MAX_PREVIEW_IMAGE_HEIGHT);
            assert_eq!(stride, width as usize * BGRA_BYTES_PER_PIXEL);
            assert_eq!(length, stride * height as usize);
        }
    }
}

pub fn exercise_svg(data: &[u8]) {
    let Ok(source) = std::str::from_utf8(data) else {
        return;
    };
    let animation = svg::is_animated(source);
    let Ok(rendered) = svg::render(source) else {
        return;
    };

    assert_eq!(animation, Ok(rendered.animated));
    assert!(!rendered.frames.is_empty());
    assert!(rendered.frames.len() <= svg::MAX_VECTOR_FRAMES as usize);
    for frame in &rendered.frames {
        let (_, expected) = checked_bgra_layout(rendered.width, rendered.height)
            .expect("an accepted SVG frame must retain a valid bounded layout");
        assert_eq!(frame.len(), expected);
    }
}

fn little_endian_u32(data: &[u8], offset: usize) -> u32 {
    let mut bytes = [0_u8; 4];
    if let Some(available) = data.get(offset..) {
        let length = available.len().min(bytes.len());
        bytes[..length].copy_from_slice(&available[..length]);
    }
    u32::from_le_bytes(bytes)
}

fn check_fitted_layout(source_width: u32, source_height: u32) {
    let Some((width, height)) = fitted_preview_dimensions(source_width, source_height) else {
        assert!(source_width == 0 || source_height == 0);
        return;
    };

    assert!(width > 0 && height > 0);
    assert!(width <= source_width && width <= MAX_PREVIEW_IMAGE_WIDTH);
    assert!(height <= source_height && height <= MAX_PREVIEW_IMAGE_HEIGHT);
    checked_bgra_layout(width, height).expect("every fitted preview must have a valid BGRA layout");
}

#[cfg(test)]
mod tests {
    use super::{
        exercise_content_sniff, exercise_layout, exercise_payload, exercise_protocol, exercise_svg,
    };
    use crate::{
        Generation, LegacyEncoding,
        payload::{PreviewResult, ResolverStatus, encode_result},
        protocol::{SessionNonce, WorkerMessage, write_message},
    };

    #[test]
    fn harnesses_accept_representative_valid_and_malformed_inputs() {
        let mut frame = Vec::new();
        write_message(
            &mut frame,
            WorkerMessage::Hello {
                nonce: SessionNonce::from_bytes([0x5a; 16]),
                cache_entries: crate::protocol::DEFAULT_PREVIEW_CACHE_ENTRIES,
                legacy_encoding: LegacyEncoding::Auto,
            },
        )
        .unwrap();
        exercise_protocol(&frame);
        exercise_protocol(b"CPWK");

        let status = encode_result(&PreviewResult::Status(ResolverStatus::Resolved)).unwrap();
        exercise_payload(&status);
        exercise_payload(&[0xff; 7]);

        exercise_content_sniff(b"\x01\xef\xbb\xbfhello");
        exercise_layout(
            &[
                0x80, 0x07, 0, 0, 0x38, 0x04, 0, 0, 0xff, 0xff, 0xff, 0xff, 1, 0, 0, 0,
            ][..],
        );
        exercise_svg(
            b"<svg viewBox='0 0 16 16'><rect width='16' height='16'>\
              <animate attributeName='x' values='0;1' dur='100ms'/></rect></svg>",
        );
        exercise_svg(b"<svg><script>unsafe()</script>");

        let mut result_frame = Vec::new();
        write_message(
            &mut result_frame,
            WorkerMessage::PreviewResult {
                generation: Generation::from_raw(u64::MAX),
                target_bounds: None,
                result: PreviewResult::Status(ResolverStatus::Unavailable),
            },
        )
        .unwrap();
        exercise_protocol(&result_frame);
    }
}
