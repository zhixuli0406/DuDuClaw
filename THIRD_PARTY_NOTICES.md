# Third-party notices for local speech synthesis

Kokoro assets are downloaded only when requested. No model weights or voice
embeddings are redistributed in the DuDuClaw binary.

| Component | Copyright / author | License | Use |
| --- | --- | --- | --- |
| Kokoro-82M v1.1-zh weights and voice embeddings | hexgrad | Apache-2.0 | Request download from the pinned onnx-community conversion, revision `6cc0f0d2ebe369a68b0df87c2b65c1af8c0ac3e3` |
| misaki | hexgrad | Apache-2.0 | Modified Rust ports of `zh.py`, `zh_frontend.py`, and `tone_sandhi.py`; model vocabulary; request-downloaded English `us_gold.json`, revision `fba1236595f2d2bf21d414ba6e57d25256afada3` |
| PaddleSpeech | Copyright (c) 2021 PaddlePaddle Authors. All Rights Reserved. | Apache-2.0 | Modified frontend and tone-sandhi algorithms, via misaki |
| jieba-rs 0.7.1 | messense and contributors | MIT | Chinese word segmentation, default dictionary |
| pinyin 0.10.0 | mozillazg and contributors | MIT | Character-to-pinyin conversion |
| ort 2.0.0-rc.12 | pyke.io and contributors | MIT OR Apache-2.0 | Existing workspace dependency, now also used directly by gateway |

Upstream sources:

- [Kokoro-82M v1.1-zh](https://huggingface.co/hexgrad/Kokoro-82M-v1.1-zh)
- [Pinned ONNX conversion](https://huggingface.co/onnx-community/Kokoro-82M-v1.1-zh-ONNX/tree/6cc0f0d2ebe369a68b0df87c2b65c1af8c0ac3e3)
- [misaki](https://github.com/hexgrad/misaki)
- [PaddleSpeech](https://github.com/PaddlePaddle/PaddleSpeech)
- [jieba-rs](https://github.com/messense/jieba-rs)
- [pinyin](https://github.com/mozillazg/rust-pinyin)

The ported Rust files identify their sources and modifications in their headers.
The Apache-2.0 license text is included at
`crates/duduclaw-gateway/src/tts_kokoro/LICENSE-APACHE`.
