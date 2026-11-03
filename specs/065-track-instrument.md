# SPEC-065: выбор инструмента на дорожке (PR 4 сэмплера)

Статус: ready
Владелец: saytomorrow
Для агента: разрешено реализовывать

Файлы: `specs/065-track-instrument.md` (новый),
`crates/audio-core/src/session.rs`, `crates/audio-core/src/project.rs`,
`crates/audio-core/tests/instrument.rs` (новый),
`crates/daw-app/src/lib.rs`, `crates/daw-gui/src/lib.rs`,
`crates/daw-gui/src/shell.rs`, `crates/daw-gui/src/main.rs`,
`crates/daw-gui/tests/undo.rs` (тесты смены инструмента; headless-тест
пикера — inline-модуль `instrument_picker_tests` в `main.rs`, т.к. `App`
приватна для бинарного крейта).

Ссылка: `docs/rfc/001-sampler-sfz.md`, разделы 5 (где что живёт), 6
(интерфейс выбора инструмента) и таблица «Фазы и PR» (PR 4). Это PR 4
вертикального среза: у MIDI/инструментальной дорожки появляется поле
`instrument`, выбор его в инспекторе и предрендер MIDI-клипа через выбранный
инструмент. Переиспользует парсер (`sfz.rs`, SPEC-062), движок
(`SamplerEngine`, SPEC-063), загрузку банка (`sample_bank.rs`) и трейт
`Instrument` (SPEC-064) как есть — без дублирования. Без живой ноды,
`SamplerNode`, сети и скачивания (PR 5–7).

## Проблема

SPEC-064 дал `sample_bank::load_bank` (загрузка локальной SFZ-папки в память)
и трейт `Instrument` (`PianoInstrument` — встроенное пианино-фолбэк,
`SamplerInstrument` — загруженный банк). Сейчас предрендер MIDI-клипа в
`load_session_cached` жёстко вызывает `piano::render_piano`, а у дорожки нет
поля, хранящего выбранный инструмент. Нужно (1) добавить в модель сессии
поле инструмента с обратной совместимостью формата проекта, и (2) дать
пользователю выбирать инструмент на дорожке в инспекторе, чтобы предрендер
MIDI-клипа шёл через выбранный инструмент.

## Поведение

### 1. Модель сессии (`audio-core`)

```rust
/// Инструмент инструментальной (MIDI) дорожки (SPEC-065 / RFC-001 §5).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum TrackInstrument {
    /// Встроенное пианино (Salamander fallback); умолчание и обратная
    /// совместимость со старыми файлами.
    #[default]
    BuiltinPiano,
    /// Локальная SFZ-папка, загруженная через `sample_bank::load_bank`.
    Sampler { bank_path: String },
}
```

`Track` получает поле `pub instrument: TrackInstrument` (умолчание
`BuiltinPiano`), заполняемое в `Track::new` / `Track::instrument`. Поле —
чистые данные без I/O; `audio-core` не знает про файлы/загрузку.

### 2. Формат проекта (project.rs, hand-written JSON)

- При записи `BuiltinPiano` **не пишется** (как `kind: "audio"` и прочие
  дефолты), поэтому старые документы при повторном сохранении остаются
  байт-в-байт прежними. `Sampler` пишется объектом:
  `"instrument": { "sampler": "<bank_path>" }`.
- При чтении отсутствующее поле даёт `BuiltinPiano` (правило «новые поля
  читаются с умолчаниями»). Неизвестное/невалидное значение — ошибка
  `UnknownField`/`Malformed` как у остальных полей.
- `bank_path` — путь к папке банка как его выбрал пользователь (абсолютный
  путь из диалога выбора папки в v1). Длина ограничена `MAX_SOURCE_BYTES`.

### 3. Предрендер через выбранный инструмент (daw-app)

`load_session_cached` в ветке `clip.notes` строит `Box<dyn Instrument>` по
`track.instrument` (вместо прямого вызова `piano::render_piano`):
- `BuiltinPiano` → `PianoInstrument` (выход бит-в-бит прежний).
- `Sampler { bank_path }` → `load_bank(path, DEFAULT_RAM_BUDGET_MIB)` затем
  `SamplerInstrument::new`; ошибки загрузки превращаются в `LoadError`
  (новый вариант, напр. `LoadError::Bank { track, source }`). Сэмплер
  рендерит на фиксированных 48 кГц (`SamplerInstrument::SAMPLE_RATE`); при
  несовпадении с частотой проекта — `LoadError::Synth` (несоответствие
  частоты — уже возвращается как `SynthError::InvalidSampleRate`).

Кэш нот (`ClipCache::notes` / `store_notes`) получает идентификатор
инструмента в ключе (RFC-001 §5: «ключ `ClipCache::notes` расширяется id
инструмента»), поэтому смена инструмента или нот даёт новый ключ и честный
перерендер без дублирования реализации — это тот же путь/кэш, что и сейчас.
`Instrument::id()` (`"piano"` / `"sampler"`) — не уникален для разных банков,
поэтому ключ дополняется `bank_path` (для сэмплера — полный путь, для
пианино — `"piano"`).

Загрузка банка и предрендер — на контрольном потоке; в аудиоколбэк ничего из
этого не вызывается (ограничения §3).

### 4. GUI: выбор инструмента в инспекторе

В `track_body` (инспектор дорожки) для `TrackKind::Instrument` добавляется
ряд «Инструмент» с выбором:

- Кнопка/комбобокс «Встроенное пианино» (всегда доступен).
- Пункт «Сэмплер: выбрать SFZ-папку…» — открывает `rfd::FileDialog`
  `pick_folder` (только локальная папка, без скачивания). После выбора папка
  загружается через `sample_bank::load_bank` **в фоне** (`std::thread::spawn`
  + `std::sync::mpsc`, результат опрашивается раз в кадр — тот же приём, что
  у фонового импорта), команда смены инструмента применяется к `Editor` как
  один undo-шаг.
- Пока банк грузится — состояние «Загрузка…» (не блокирует UI).
- После загрузки показывается имя банка (имя `.sfz`-файла или папки), статус
  загрузки и читаемое сообщение о предупреждениях/ошибках: число
  предупреждений (`warnings.len()`), число пропущенных регионов
  (`decode_failures.len()`), превышение бюджета RAM
  (`BankLoadError::RamBudgetExceeded`), отсутствующий `.sfz`
  (`BankLoadError::NoSfz`) и т.п.

Смена инструмента — одна undoable-команда: `Editor::set_track_instrument`
(по образцу `set_track_format`), фиксирует сессию через `commit_session`.
После смены инструмента кэш рендера недействителен автоматически (ключ
изменился).

### 5. Интеграция (минимум)

Живой `SamplerNode` не добавляется. Воспроизведение MIDI-клипов (playback и
export) продолжает идти через `load_session_cached`; теперь он выбирает
инструмент по `track.instrument` и перерендеривает при смене нот или
инструмента.

## Граничные случаи

| Случай | Ожидаемое поведение |
|---|---|
| старый файл без поля `instrument` | `BuiltinPiano`, повторное сохранение не меняет текст |
| `BuiltinPiano` сохранён | поле не пишется, текст совпадает со старым |
| `Sampler` сохранён | пишется `"instrument": {"sampler": "…"}`, читается обратно |
| `bank_path` не существует / нет `.sfz` | предрендер → `LoadError::Bank` (`NoSfz`) |
| банк превышает бюджет RAM | предрендер → `LoadError::Bank` (`RamBudgetExceeded`) |
| сэмпл банка отсутствует | регион в `decode_failures`, банк грузится, звук без него |
| `sample_rate` проекта ≠ 48 кГц у сэмплера | `LoadError::Synth` (`InvalidSampleRate`) |
| дорожка `Audio` | поле `instrument` игнорируется, предрендер не задействован |
| смена нот | новый ключ кэша, перерендер |
| смена инструмента | новый ключ кэша, перерендер |

## Критерии приёмки

- [ ] `cargo fmt --all --check` зелёный
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` зелёный
- [ ] `cargo test --workspace --all-targets` зелёный (offline-путь; фича
      `realtime` не собирается локально из-за отсутствия `libasound2-dev`,
      проверяется CI-джобой «realtime backend build»)
- [ ] Модель: round-trip `BuiltinPiano`/`Sampler`, старый файл без поля
      читается как `BuiltinPiano`, повторное сохранение старого файла не
      меняет текст.
- [ ] Undo/redo смены инструмента (одна команда, восстанавливает прежний
      инструмент).
- [ ] Headless-тест App: состояние пикера (выбран «Встроенное пианино» по
      умолчанию, выбор сэмплера меняет `track.instrument`).
- [ ] Предрендер: `BuiltinPiano` даёт выход бит-в-бит равный
      `piano::render_piano` (goldens не меняются); `Sampler` рендерит через
      `SamplerInstrument` и выбирает верный инструмент по дорожке.
- [ ] Не ослабляются существующие тесты; goldens не трогаются.

## Нельзя

- Менять `crates/audio-core/src/sampler/*` (SPEC-062/063), goldens,
  `crates/audio-core/tests/sampler*.rs`.
- Добавлять `SamplerNode` или живую ноду в реалтайм-путь (PR 5–7).
- Трогать `vendor/winit`, `SOUND_PRO_PATCH.md`, `x11_cursor.rs`,
  `tests/drop.rs`, `docs/PROJECT_STATE.md`, `docs/ROADMAP.md`.
- Хардкодить высоты строк GUI: геометрия строк — единственный источник в
  `track_rows.rs`.
- Использовать `unsafe`, аллокации/блокировки/I/O в аудиоколбэке (загрузка и
  предрендер — вне колбэка).
- Добавлять сеть/скачивание (PR 5–6).

## Вне области

`bank_library.rs` (каталог, манифесты), `bank_download.rs` (HTTP + SHA-256,
PR 6), живая нода `SamplerNode`, диалог первого запуска «Скачать звуки
сейчас / Позже», окно «Авторы» вместо `PIANO_CREDIT` — PR 5–7 RFC-001.
