PaddleOCR's PP-OCRv3 text detector and recogniser (Chinese+English, 6,623
characters), as the Python sc-watch runs them through RapidOCR 1.2.3
(rapidocr-onnxruntime, which ships these same files):

  ch_PP-OCRv3_det_infer.onnx  sha1 61be59ddd4f6d02193e00e0cf8726effa2a6947a
  ch_PP-OCRv3_rec_infer.onnx  sha1 654bd482354c4df3406d18e5d6eece274062f53f
  ppocr_keys.txt              the recogniser's characters, one a line, as
                              stored in its own metadata ("character")

PaddleOCR: https://github.com/PaddlePaddle/PaddleOCR
RapidOCR:  https://github.com/RapidAI/RapidOCR
Both under the Apache License 2.0 (LICENSE-APACHE beside this file).
Copyright (c) 2020 PaddlePaddle Authors.

scw-ocr runs them with rten (pure Rust) for the legacy OCR reader and for
chat reads. Unchanged from the files RapidOCR ships.
