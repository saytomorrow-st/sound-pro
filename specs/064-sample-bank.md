# SPEC-064: загрузка банка сэмплов + трейт Instrument + предрендер через сэмплер

Статус: ready
Владелец: saytomorrow
Для агента: разрешено реализовывать

Файлы: `crates/daw-app/src/sample_bank.rs` (новый),
`crates/daw-app/src/instrument.rs` (новый), `crates/daw-app/src/lib.rs`,
`crates/daw-app/src/piano.rs`, `crates/daw-app/tests/sample_bank.rs` (новый),
`crates/daw-app/tests/instrument.rs` (новый).

Ссылка: `docs/rfc/001-sampler-sfz.md`, разделы 3.3 (preload и бюджет RAM),
4 (реалтайм-анализ), 5 (где что живёт) и таблица «Фазы и PR» (PR 3).
Это PR 3 вертикального среза: загрузка банка, трейт `Instrument`, пианино за
ним, предрендер клипа через сэмплер. Без `SamplerNode`, hot_path-проводки,
GUI, сети, скачивания и изменения формата проекта (всё это — PR 4+).

## Проблема

SPEC-062 дал парсер SFZ (`audio_core::sampler::sfz`), SPEC-063 — движок голосов
(`SamplerEngine`), который принимает уже декодированные в память `i16`-сэмплы
(`Arc<[i16]>`) и разрешённые `SampleRegion`. Теперь нужно (1) из локальной
папки SFZ-банка собрать эти данные на контрольном потоке, и (2) дать единый
трейт `Instrument`, через который офлайн-предрендер MIDI-клипа работает и для
встроенного пианино, и для загруженного банка. Парсер и движок переиспользуются
как есть — не дублируются и не меняются.

## Поведение

### 1. Загрузка банка (sample_bank.rs)

```rust
pub const DEFAULT_RAM_BUDGET_MIB: u64 = 512;

pub struct LoadedBank {
    pub regions: Vec<SampleRegion>,
    pub samples: Vec<std::sync::Arc<[i16]>>,
    pub warnings: Vec<String>,        // предупреждения парсера + по регионам
    pub decode_failures: Vec<RegionFailure>,
    pub ram_bytes: u64,               // фактические байты сэмплов
}

pub struct RegionFailure {
    pub sample: String,               // путь сэмпла, как в .sfz
    pub reason: String,
}

pub fn load_bank(dir: &Path, budget_mib: u64) -> Result<LoadedBank, BankLoadError>;
```

`load_bank`:

1. Читает единственный `.sfz` в `dir` (если их несколько — берёт первый,
   предупреждение; если нет ни одного — ошибка). Текст `.sfz` читается как
   UTF-8.
2. Парсит через `audio_core::sampler::sfz::parse` с `IncludeResolver`, который
   читает `#include`-файлы относительно `dir` и запрещает выход за его пределы
   (абсолютные пути и `..` уже отклоняются парсером как `UnsafePath`).
3. Для каждого региона резолвит путь сэмпла: `sample` относительно
   `default_path` (если есть), затем относительно `dir`. Декодирует через
   `audio_file::read_audio` (symphonia: WAV/FLAC/OGG/MP3; системных библиотек
   не добавляется) и переводит в `i16`: моно — `Arc<[i16]>`, стерео —
   интерливированный `[L,R,L,R,…]` (`Arc<[i16]>`), лишние каналы сводятся к
   стерео/моно. Пересчёт `f32` → `i16` масштабом `s * 32767.0` с сатурацией.
4. Считает бюджет RAM: `frames · channels · 2` байт на сэмпл. Если сумма
   превышает `budget_mib`, возвращает `BankLoadError::RamBudgetExceeded` (с
   нужным и фактическим объёмом) — банк целиком не загружается, без OOM.
5. Разрешает регионы через `audio_core::sampler::region::resolve` (таблицу
   ключ→регион `RegionTable` строит `SamplerEngine::new` из этих регионов);
   регион с отсутствующим/нечитаемым сэмплом регистрируется в
   `decode_failures` и пропускается (не роняет весь банк). Пустые (нулевой
   длины) сэмплы тоже пропускаются.
6. Собирает `LoadedBank` с `regions`, `samples`, `warnings` и
   `decode_failures`; предупреждения парсера (`SfzWarning`) и отброшенные при
   загрузке регионы попадают в `warnings`.

Вся загрузка и декодирование — на контрольном потоке (I/O, аллокации
разрешены); в аудиоколбэк эта функция не вызывается.

### 2. Трейт Instrument (instrument.rs)

```rust
pub trait Instrument {
    fn id(&self) -> &str;
    fn render_notes(&self, notes: &[Note], sample_rate: u32, frames: u64)
        -> Result<Vec<f32>, SynthError>;
}
```

- `render_notes` — офлайн-предрендер списка нот клипа в моно-буфер `frames`
  отсчётов на контрольном потоке (по образцу `piano::render_piano` и
  `synth::render_notes`).
- Встроенное пианино реализует трейт без изменения выхода: `PianoInstrument`
  делегирует в `piano::render_piano`; `id() == "piano"`. Существующие golden
  эталоны и поведение пианино остаются бит-в-бит прежними.
- `SamplerInstrument` оборачивает `SamplerEngine` и `LoadedBank` и реализует
  тот же трейт: строит `Schedule` из нот (NoteOn/NoteOff, как в
  `synth::render_notes`), рендерит стерео через `SamplerEngine::render`
  квантами и сводит в моно `(L + R) · 0.5` (тот же микс, что и пианино).
  `id() == "sampler"`.

### 3. Проводка (минимум)

- `piano::render_piano` остаётся публичным и неизменным; вызов в
  `load_session_cached` (ветка `clip.notes`) продолжает работать как раньше
  (выход не меняется). Трейт вводится как **дополнительный** путь
  предрендера; какая-либо замена горячего пути, `SamplerNode` или живой ноды
  в этом PR не делается.
- Документированная точка входа — публичные `sample_bank::load_bank` и
  `instrument::{Instrument, PianoInstrument, SamplerInstrument}`; интеграция с
  `SessionJob`/GUI/форматом проекта — PR 4+.

## Граничные случаи

| Случай | Ожидаемое поведение |
|---|---|
| нет `.sfz` в папке | `BankLoadError::NoSfz` |
| несколько `.sfz` | берётся первый, предупреждение |
| `.sfz` не парсится | `BankLoadError::Sfz` с причиной |
| сэмпл отсутствует / не декодируется | регион в `decode_failures`, банк грузится |
| сэмпл нулевой длины | регион пропускается, банк грузится |
| суммарный объём > бюджета | `BankLoadError::RamBudgetExceeded`, банк не грузится |
| `budget_mib == 0` | ошибка бюджета (не OOM) |
| `#include` вне папки | ошибка парсера (`UnsafePath`) пробрасывается |
| пустой список нот / `sample_rate == 0` | `render_notes` → `SynthError::InvalidSampleRate` |
| нота вне всех регионов | голос не стартует, выход молчит (без паники) |

## Критерии приёмки

- [ ] `cargo fmt --all --check` зелёный
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` зелёный
- [ ] `cargo test --workspace --all-targets` зелёный (offline-путь; фича
      `realtime` не собирается локально из-за отсутствия `libasound2-dev`,
      проверяется CI-джобой «realtime backend build»)
- [ ] Тесты на синтетических SFZ + генерируемом в `temp_dir` WAV (без реальных
      банков и бинарников в репозитории): загрузка банка, превышение бюджета,
      отсутствующий сэмпл, предрендер даёт ожидаемую высоту и тайминг нот.
- [ ] Тест, что выход `PianoInstrument` совпадает с `piano::render_piano`
      (бит-в-бит) — пианино-фолбэк не меняет звук.

## Нельзя

- Менять `crates/audio-core/src/sampler/*` (SPEC-062/063), goldens,
  `crates/audio-core/tests/sampler*.rs`.
- Добавлять `SamplerNode`, hot_path-проводку, GUI, сеть, скачивание, изменение
  формата проекта.
- Трогать `vendor/winit`, `SOUND_PRO_PATCH.md`, `x11_cursor.rs`,
  `tests/drop.rs`, `docs/PROJECT_STATE.md`, `docs/ROADMAP.md`.
- Добавлять системные библиотеки или тяжёлые зависимости; декодирование — через
  существующий symphonia-декодер из SPEC-053.
- Использовать `unsafe`, аллокации/блокировки/I/O на горячем пути (загрузка и
  предрендер — вне колбэка).

## Вне области

`bank_library.rs` (каталог банков, манифесты, суммы), `bank_download.rs`
(HTTP + SHA-256, PR 6), `SamplerNode`/живая нода, выбор инструмента в
инспекторе, сериализация `Track.instrument` — это PR 4–7 RFC-001.
