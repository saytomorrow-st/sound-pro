# Архитектура Sound Pro

## Слои

```
crates/daw-gui   окно (eframe/egui): таймлайн, микшер, инспектор, диалоги
      │  команды, снапшоты сессии
crates/daw-app   оболочка: файлы, PCM-кэш, метры, бэкенды, CLI
      │  Graph, Renderer, Session, ParamHandle
crates/audio-core движок: без платформенных API, #![forbid(unsafe_code)]
```

Зависимость только в одну сторону: GUI → app → core. `audio-core` не знает ни
про ОС, ни про устройства.

## audio-core

| Модуль | Ответственность |
|---|---|
| `buffer` | `AudioBuffer`, тип сэмпла |
| `transport` | Позиция, темп, цикл, команды транспорта |
| `graph`, `render` | Граф узлов и рендерер офлайн/реалтайм-пути |
| `offline` | Рендер проекта в буфер без устройства |
| `session` | Модель сессии: дорожки, клипы, шины, маркеры, автоматизация |
| `project` | Сериализация в текстовый JSON и разбор |
| `fx` | EQ, компрессор, гейт, лимитер, реверб; `EffectParam` (38 параметров) |
| `params`, `commands` | Спецификации параметров, SPSC-очередь команд |
| `schedule`, `midi` | Планировщик событий, SMF-парсер |
| `synth` | Полифонический синтезатор |
| `sampler` | SFZ-сэмплер: парсер, движок голосов, регионы, стриминг |
| `sampler::stream` | `VoiceStream`/`SampleHead`: голова в RAM, хвост через SPSC-кольца |
| `resample` | Точный ресемплер (воспроизведение) и быстрый (импорт) |
| `wav` | Чтение/запись WAV, инкрементальный рекордер |

## Контракт реального времени

Выполняется для `Graph::process`, `Renderer::render` и аудиоколбэка:

- никаких аллокаций (`Vec::push`, `Box::new`, `format!`, `collect`);
- никаких блокировок (`Mutex`, `RwLock`, блокирующие каналы);
- никакого I/O (файлы, сеть, env, `println!`);
- никаких паник на неверном входе — только `Result` (`GraphError`, `RenderError`).

Всё дорогое — в конструкторе или в фоновом потоке UI. Обмен — через `rtrb`
(SPSC-рингбуфер), выделенный заранее; переполнение приводит к отбрасыванию
команды, не к панике.

## daw-app

- `audio_file` — чтение WAV/MP3/FLAC/OGG, ресемплинг при импорте, прогресс по пакетам.
- `pcm_cache` — дисковый кэш PCM импортированных аудиофайлов: `~/.cache/sound-pro/pcm`,
  ключ путь+mtime+размер+частота, до 16 записей, атомарная запись.
- `bank_cache` — PCM-кэш сэмпл-банков (RFC-002 §5): `<bank>/cache/*.pcm`
  (64-байтовый `SPCM`-заголовок + interleaved i16) и `manifest.json` с
  хэшами xxh3-128/blake3/sampled-xxh3, mtime и размерами для проверки целостности.
- `bank_integrity` — проверка целостности кэша уровнями L0–L3, карантин
  (`*.quarantined`) и автоперестроение из исходных FLAC.
- `bank_library`, `bank_download` — библиотека банков, каталог, скачивание
  с проверкой лицензии; при скачивании кэш генерируется сразу.
- `sample_bank` — загрузка банка: выбор Resident/Streaming по бюджету RAM,
  `load_bank_auto`, `load_bank_from_cache(_streaming)`.
- `stream_loader` — `StreamLoader`: читает хвосты сэмплов из `.pcm` (`pread`)
  в кольца голосов; синхронный `pump` (офлайн) и фоновый поток (реалтайм).
- `meminfo` — `MemAvailable` из `/proc/meminfo` и `current_rss_mib`; расчёт
  эффективного бюджета RAM с запасом 256 МиБ и полом 512 МиБ.
- `nodes`, `meters` — узлы графа и банки измерений без аллокаций в аудиопути.
- `audio_settings`, `realtime` — выбор устройства (CPAL, фича `realtime`).
- `piano` — сэмплы пианино.
- `capture`, `playback` — запись и воспроизведение на уровне сессии.

## daw-gui

- `main` — `App`: состояние окна, обработка ввода, drag-and-drop, фоновый
  импорт, отрисовка.
- `shell` — панель управления, инспектор, микшер, браузер (SPEC-034).
- `timeline` — регионы, сетка тактов, курсор, линейка.
- `layout` — чистые функции раскладки (фейдеры, метры, подписи), легко тестируются.
- `lib` — `Editor` (модель без egui), `drop_target`, `DropTracker`, пики волны;
  всё, что можно проверить юнит-тестом, живёт здесь.
- `audio_editor` — редактор волны нижней панели.

Принцип: логика — в чистых функциях `lib.rs`, а `main.rs` только связывает их с
egui. Тесты в `crates/daw-gui/tests/*.rs` работают без окна.

## Стриминг сэмплов (RFC-002)

Выбор режима при загрузке банка (`sample_bank::load_bank_auto`):

1. `meminfo::effective_budget_mib` считает бюджет RAM: `min(DEFAULT_RAM_BUDGET_MIB,
   MemAvailable − 256 МиБ)`, но не ниже пола 512 МиБ.
2. `load_bank_from_cache` читает весь PCM-кэш в RAM (режим `Resident`).
3. Если резидентные данные превышают бюджет (`RamBudgetExceeded`), грузимся
   повторно в `Streaming` (`load_bank_from_cache_streaming`): каждый стерео-сэмпл
   длиннее `STREAM_HEAD_FRAMES = 32_768` кадров держит в RAM только голову
   (`SampleSource::Streaming(SampleHead)`), хвост читается с диска.

Сэмпл-источник — `SampleSource::{Resident, Streaming}` (SPEC-069). Голова и
хвост описаны в `audio_core::sampler::stream`: `VoiceStream` на голос читает
голову из RAM, а после неё запрашивает блоки хвоста через `LoadRequest` и
получает их через пару SPSC-колец `rtrb` (сэмплы + `BlockHeader`). Константы:
`HEAD_FRAMES = 65_536` (номинал движка), фактическая голова банка —
`STREAM_HEAD_FRAMES = 32_768`; `RING_FRAMES = 32_768` кадров на голос.
Аудиопоток при этом не аллоцирует, не блокирует и не делает I/O — всё чтение с
диска идёт в `StreamLoader`.

`StreamLoader` (в `daw-app`) отвечает на `LoadRequest`, читая хвост из
`*.pcm` (`pread`/`seek_read`, без `mmap`). Два привода: синхронный `pump`
(офлайн-рендер `SamplerInstrument` и тесты — детерминированный «SyncStream»,
SPEC-074) и фоновый поток `spawn` для реалтайма. Запросы обслуживаются в
порядке поступления (FIFO), сортировки по дедлайну нет. Один loader держит до
`MAX_OPEN_FILES = 256` открытых дескрипторов.

## Потоки

```
UI (egui)  ──команды──▶  rtrb  ──▶  аудиопоток (CPAL callback → Graph::process)
    ▲                                    │
    └──────── метры/позиция ─── rtrb ◀────┘
```

Импорт и декодирование — отдельный поток: результат приходит в UI через
`std::sync::mpsc`, UI опрашивает канал раз в кадр.

## PCM-кэш банка и целостность (RFC-002 §5–§6)

Кэш хранится под `~/.local/share/sound-pro/banks/<id>/cache/`
(`%LOCALAPPDATA%\sound-pro\banks\<id>\cache\` на Windows):

- `*.pcm` — по одному файлу на уникальный сэмпл: 64-байтовый little-endian
  заголовок (`SPCM`-magic, `format_version=1`, каналы, частота, кадры,
  `src_flac_size`, `src_flac_xxh3`, CRC-32) + interleaved i16.
- `manifest.json` — атомарно (`.tmp` + `rename`): `format_version`, `bank_id`,
  `total_bytes`, записи `entries[]` (`sample_id`, `src`, `cache`, `size`,
  `xxh3_128`, опц. `blake3`/`sampled_xxh3`, частота/каналы/кадры, `src_size`,
  `src_xxh3`, `mtime_ns`). Устойчивый ключ — `src`, а не `sample_id`.

Кэш — производные данные: удаление `cache/` безопасно, источник правды — FLAC.

Проверка целостности при каждой загрузке (`bank_integrity`), уровни L0–L3
(каждый — надмножество предыдущего):

| Уровень | Когда | Что проверяет |
|---|---|---|
| L0 | всегда, синхронно | манифест `format_version`, существование, размер `64+frames×ch×2`, заголовок (magic/version/CRC, rate/ch/frames) |
| L1 | всегда, фоном | mtime источника + sampled xxh3-64 (первые/средние/последние 64 КиБ) |
| L2 | фоном, раз за сессию и после сбоя (`cache/.dirty`) | полный xxh3-128 каждого `.pcm` |
| L3 | по запросу/после сбоя | blake3 `.pcm` + xxh3-64 исходного FLAC |

Повреждённый файл помечается `*.quarantined` и пересобирается из исходного
FLAC (`recover_bank_cache`, 1–2 потока низкого приоритета); при отсутствии
исходника — `unrecoverable`, UI предлагает скачать банк заново.

## Хранение проекта

Текстовый JSON (SPEC-008): фиксированный порядок ключей, разбор с ошибкой на
опечатку. Новые поля читаются с умолчаниями для старых файлов (например,
`knee_db` пишется только если не ноль, старые проекты дают моно и стерео-флаг
по умолчанию).

## Тестирование

| Уровень | Где | Что проверяет |
|---|---|---|
| Юнит | внутри модулей | чистые функции, валидация, DSP-мелочи |
| Интеграционные | `crates/*/tests/*.rs` | сессия, рендер, редактирование, раскладка |
| Golden | `crates/audio-core/tests/golden/*.wav` | оракул DSP; менять нельзя |
| CLI | `daw selftest`, `daw bounce` | работа без звуковой карты |

## Сборка и выпуск

- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --all-targets` — обязательный гейт.
- CI: ubuntu + windows, goldens, сборка с фичей `realtime`.
- Release workflow: тег `vX.Y.Z` → AppImage и zip для Windows; версия из тега.
