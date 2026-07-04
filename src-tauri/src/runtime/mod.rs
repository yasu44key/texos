// 実行環境管理（Ollama / Whisper / t2i デーモン）。WS2 で python.rs（uv/venv 管理）を追加予定。
pub(crate) mod ollama;
pub(crate) mod t2i;
pub(crate) mod whisper;
pub(crate) use ollama::*;
pub(crate) use t2i::*;
pub(crate) use whisper::*;
