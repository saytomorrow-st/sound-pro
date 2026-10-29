# Sound Pro

Каркас открытой DAW на Rust для Linux и Windows, спроектированный так, чтобы с
ним могли продуктивно работать ИИ-агенты: движок тестируется полностью без
звуковой карты, правила для агентов явно записаны, а эталоны звука защищены от
случайной перегенерации.

## Структура

```
crates/audio-core   движок: часы, буферы, граф, renderer, offline, WAV, MIDI
crates/daw-app      оболочка: CLI smoke-инструменты, фича realtime (CPAL)
specs/              спецификации; агенты реализуют только то, что здесь описано
docs/               REALTIME.md, MIDI.md (SMF), SYNTH.md (голоса), SESSION.md (клипы), AI-WORKFLOW.md
clean-room/         журнал метода чистой комнаты
AGENTS.md           правила для ИИ-агентов (читать первым)
.github/            CI, инструкции и промпты Copilot, шаблон задачи
.vscode/            задачи, отладка, настройки rust-analyzer/Copilot
```

## Настройка среды

См. `docs/DEVSETUP.md`: инструменты, расширения VS Code, как подключить
инструкции и промпты GitHub Copilot, первый запуск и типовые ошибки.

## Быстрый старт

```bash
cargo test --workspace --all-targets
cargo run -q --bin daw -- selftest
cargo run -q --bin daw -- bounce tone.wav 1.0
cargo run -q --bin daw -- bounce take.wav 30.0 --split 48000   # части по 1 с
```

Реальный аудиовыход (CPAL; Linux: `libasound2-dev`, Windows: без зависимостей):

```bash
cargo build -p daw-app --features realtime
```

## Что уже работает

- Транспорт: play/stop/seek/rewind, темп, тактовый размер, тики PPQN 960.
- Граф: резервируемая ёмкость, суммирование, ошибки вместо паник на горячем пути.
- Offline-рендер с независимостью от размера кванта; golden-тесты.
- WAV float32/PCM16, CLI-смок (`selftest`, `bounce`).
- Инкрементальная запись (`Recorder`) с разбиением на части — длинный bounce
  не держится в памяти целиком (SPEC-002).
- SPSC-очередь команд UI → аудио (SPEC-003), парсер MIDI SMF (SPEC-004).
- Realtime-бэкенд на CPAL (`daw play`, фича `realtime`) — SPEC-005.
- [Документ проекта](docs/PROJECT.md): сессия как JSON без `serde`
  (`daw project save|show`) — SPEC-008.
- [Полифонический синтезатор](docs/SYNTH.md): 16 голосов, ADSR, MIDI-файл →
  WAV (`daw render-midi`) — SPEC-009.
- [Сессии и клипы](docs/SESSION.md): дорожки, окна в источники, рендер суммы
  (`daw render-session`) — SPEC-010.

## Что дальше

См. `docs/AI-WORKFLOW.md` → «Первые 5 задач для агента» и `specs/000-template.md`.
[SPEC-007](specs/007-midi-event-scheduler.md) отдаёт события в узел,
[SPEC-008](specs/008-project-document.md) сохраняет сессию, а
[SPEC-009](specs/009-polyphonic-synth.md) играет их настоящими голосами.
Ближайшее — следующий номер в `specs/`.

## Лицензия

MIT OR Apache-2.0 (код). Название и юридическая проверка — до публичного релиза.
