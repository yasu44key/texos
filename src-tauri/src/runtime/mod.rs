// 実行環境管理（Ollama / Whisper / t2i デーモン / Python env / PCスペック検出）。
pub(crate) mod ollama;
pub(crate) mod python;
pub(crate) mod t2i;
pub(crate) mod whisper;
pub(crate) use ollama::*;
pub(crate) use python::*;
pub(crate) use t2i::*;
pub(crate) use whisper::*;
