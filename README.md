# mosaik-ki

Lokale KI (Gemma 4 über llama.cpp) für die Mosaik-Apps.

## Einbindung

Das Crate nutzt Threads und TCP und baut daher nicht für `wasm32`. Apps binden es deshalb nur dort ein:

```toml
[target.'cfg(not(target_arch = "wasm32"))'.dependencies]
mosaik-ki = { git = "https://github.com/cansi798/mosaik-ki", tag = "v0.1.2", features = ["egui"] }
```

## Dateilayout

- `<install_dir>/llama` enthält das llama.cpp-Programm (`llama-server`) samt Bibliotheken.
- `<install_dir>/models` enthält die Modelldateien (Kompakt: `gemma-4-E2B_q4_0-it.gguf`).
- Die Einstellungen stehen in `mosaik-ki.json` im Mosaik-Konfigurationsordner
  (`default_config_path()`, unter Linux `~/.config/Mosaik/mosaik-ki.json`) mit `tier`, `install_dir` und `port`.

Regeln für Installer und Deinstallation:

- Modelldateien zuerst als `<datei>.part` herunterladen und erst nach vollständigem Download umbenennen.
  `KiConfig::is_ready` prüft nur, ob die Dateien existieren, ein halber Download darf also nie unter dem
  endgültigen Namen liegen.
- Vor dem Ersetzen oder Löschen von Dateien `server::stop(&cfg)` aufrufen. Der Dienst schreibt seine
  Prozessnummer nach `<install_dir>/llama-server.pid`; `stop` beendet ihn nur, wenn der Port wirklich
  einen Mosaik-Dienst beantwortet, und wartet bis zu 5 Sekunden auf das Ende.

## Schnittstellen

- `KiHost`: Die App stellt dem Seitenfenster Auswahl, Ersetzen und Rückgängig-Gruppen bereit.
- `KiTools`: Die App beschreibt ihre Werkzeuge (`definitions`) und führt Aufrufe aus (`call`).
  Die Schleife selbst ist `agent::AgentRun` bzw. `agent::run_blocking`.
- Der Dienst läuft nur auf `127.0.0.1` (Standard-Port 11535) und wird bei Bedarf von
  `server::ensure_running` gestartet.

## Vertrag für `KiHost`

1. Solange `KiPanel::locks_document()` `true` liefert (KI arbeitet oder eine übernehmbare Vorschau ist offen),
   macht die App das Dokument schreibgeschützt und ignoriert Änderungen und Auswahlwechsel.
2. Nach dem Schließen von „Mosaik einrichten“ ruft die App `KiPanel::refresh()` auf.
3. `end_undo_group(false)` stellt den Zustand wieder her, der bei `begin_undo_group` festgehalten wurde.

Eingaben sind auf 12 000 Zeichen begrenzt (auch im Agentenbetrieb inklusive Anweisung). Die Antwortlänge der
Schnellaktionen richtet sich nach der Eingabe (`actions::max_tokens_for`); wird eine Antwort abgeschnitten,
meldet das Crate `KiError::AnswerTooLong`, statt Teiltext zu übernehmen.

## Verhalten mit Gemma

Gemma liefert Werkzeugaufrufe teils im eigenen Format (`name{key:<|"|>wert<|"|>}`) und antwortet nach einem
Werkzeugaufruf oft mit leerem Text. Beides wird erkannt: das Format wird in normale Aufrufe umgewandelt, ein
leerer Text nach erledigter Arbeit wird zu „Erledigt.“, eine leere Schnellaktion zeigt eine Meldung.

## Echttest

`cargo test --test real_gemma -- --ignored --nocapture --test-threads 1` (braucht installiertes Modell).

## Lizenzen

Dieses Crate: `MIT OR Apache-2.0`. Gemma 4 steht unter Apache-2.0, llama.cpp unter MIT.
