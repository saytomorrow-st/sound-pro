# Sound Pro

Каркас открытой DAW на Rust для Linux и Windows, спроектированный так, чтобы с
ним могли продуктивно работать ИИ-агенты: движок тестируется полностью без
звуковой карты, правила для агентов явно записаны, а эталоны звука защищены от
случайной перегенерации.

## Структура

```
crates/audio-core   движок: часы, буферы, граф, renderer, offline, WAV
crates/daw-app      оболочка: CLI smoke-инструменты, фича realtime (CPAL)
specs/              спецификации; агенты реализуют только то, что здесь описано
docs/               REALTIME.md (контракт аудиопотока), AI-WORKFLOW.md
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

## Что дальше

См. `docs/AI-WORKFLOW.md` → «Первые 4 задачи для агента» и `specs/000-template.md`.

## Лицензия

MIT OR Apache-2.0 (код). Название и юридическая проверка — до публичного релиза.
