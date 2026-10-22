# Compression contract
Last updated: 2026-10-22

HTTP reports use gzip (RFC 1952) when enabled. The encoder emits fixed-Huffman
DEFLATE; the decoder accepts stored, fixed and dynamic blocks (RFC 1951).
DEFLATE packs ordinary bit fields least-significant first while Huffman symbols
are read in code order. Those are separate rules, covered by source fixtures.

WebSocket permessage-deflate is negotiated per connection. Control frames are
not compressed. Gorilla-compatible messages may end with a non-final block;
decoding adds the sync-flush trailer `00 00 FF FF` followed by the final empty
stored block `01 00 00 FF FF`. Expansion is bounded to the control-message limit.

Implementation: `src/gzip.rs`, `src/inflate.rs`, `src/ws.rs`. Tests include RFC
vectors and the server-compatible trailer. The original planning draft is
historical; current boundaries are in [Architecture](../architecture.md).
