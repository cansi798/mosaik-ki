# mosaik-ki

Lokale KI (Gemma 4 über llama.cpp) für die Mosaik-Apps.

## Einbindung

Das Crate nutzt Threads und TCP und baut daher nicht für `wasm32`. Apps binden es deshalb nur dort ein:

```toml
[target.'cfg(not(target_arch = "wasm32"))'.dependencies]
mosaik-ki = { git = "https://github.com/cansi798/mosaik-ki", features = ["egui"] }
```
