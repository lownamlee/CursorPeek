# Generated text corpus

These cases are constructed at test time by `src/worker/text/corpus.rs`. They use original string
literals, explicit Unicode encoding helpers, `encoding_rs`, or short synthetic malformed byte
sequences. No third-party fixture content is copied into the repository.

`cases.tsv` is the review manifest. Its order and identifiers are checked against the executable
table so every documented case remains active. The corpus covers multilingual scripts, combining
marks and emoji, UTF-8/16/32 byte order, explicit Windows-1252 and Shift_JIS decoding, mixed line
endings, unsafe controls and bidi formatting, exact line/scalar limits, malformed declared
Unicode, binary signatures, and control-heavy negatives.
