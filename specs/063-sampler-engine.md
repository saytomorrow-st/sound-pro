# SPEC-063: Движок голосов сэмплера (voice pool → рендер)

Статус: ready
Владелец: saytomorrow
Для агента: разрешено реализовывать

Файлы: `crates/audio-core/src/sampler/{region,voice,engine}.rs` (новые),
`crates/audio-core/src/sampler/mod.rs`, `crates/audio-core/src/lib.rs`,
`crates/audio-core/tests/sampler.rs` (новый).

Ссылка: `docs/rfc/001-sampler-sfz.md`, разделы 3 (архитектура голосов),
4 (реалтайм-анализ) и 5 (где что живёт). Это PR 2 вертикального среза: только
движок голосов и офлайн-рендер, без файлов, декодера, банков, `Instrument`,
`SamplerNode`, GUI и изменений формата проекта (всё это — PR 3+).

## Проблема

SPEC-062 дал парсер SFZ в типизированный `Vec<Region>`. Теперь нужно из этих
регионов и уже декодированных в память сэмплов (`i16`) собрать звук: выбрать
регион по ноте/velocity, назначить голос из фиксированного пула, вычислить
огибающую и прочитать сэмпл с линейной интерполяцией. Парсер переиспользуется
как есть — его `Region` и `SfzDocument` не дублируются и не меняются.

## Поведение

### 1. Типы данных (region.rs)

Новый модуль `region.rs` описывает данные, необходимые движку на горячем пути,
поверх типизированного `sfz::Region` (RFC §3.3: «в памяти — `i16` PCM,
`Arc<[i16]>`»):

```rust
/// Один сэмпл-регион, полностью разрешённый и готовый к воспроизведению.
pub struct SampleRegion {
    /// Индекс сэмпла в банке (0 — банк без сэмплов).
    pub sample_index: usize,
    /// Число каналов сэмпла (1 или 2).
    pub channels: u8,
    /// Корневая нота (pitch_keycenter), по умолчанию 60.
    pub root: u8,
    /// Нижняя/верхняя граница нот (lokey/hikey), по умолчанию 0..=127.
    pub key_lo: u8,
    pub key_hi: u8,
    /// Нижняя/верхняя граница velocity (lovel/hivel), по умолчанию 1..=127.
    pub vel_lo: u8,
    pub vel_hi: u8,
    /// Тонкая подстройка в центах (tune) + транспозиция (transpose) в полутонах.
    pub tune_cents: i32,
    /// Усиление `10^(volume/20)` (линейное), применяется после огибающей.
    pub gain: f32,
    /// Velocity tracking 0..=1 (amp_veltrack/100, по умолчанию 1.0).
    pub vel_track: f32,
    /// Панорама −1.0..=1.0 из `pan` через `audio_core::pan_law` (без дубля закона).
    pub pan: f32,
    /// Огибающая амплитуды (секунды/уровень sustain 0..=1).
    pub envelope: AmpEnvelope,
    /// Группа (group) и группа, которую регион обрезает (off_by), если есть.
    pub group: Option<i32>,
    pub off_by: Option<i32>,
    /// Как обрезается группа off_by (off_mode), по умолчанию `Normal`.
    pub off_mode: OffMode,
    /// Режим цикла и границы (loop_mode, loop_start, loop_end).
    pub loop_mode: LoopMode,
    pub loop_start: u32,
    pub loop_end: u32,
    /// Окно воспроизведения (offset, end); end == 0 означает «до конца сэмпла».
    pub offset: u32,
    pub end: u32,
}

/// Полная огибающая амплитуды ADSR в отсчётах (вычисляется при загрузке).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AmpEnvelope {
    pub attack_samples: u32,
    pub hold_samples: u32,
    pub decay_samples: u32,
    pub sustain: f32, // 0.0..=1.0 от пика
    pub release_samples: u32,
}
```

Сборка из `sfz::Region` — отдельная чистая функция (вне аудиопотока, в
конструкторе банка/движка), которая резолвит `Option`-поля к значениям по
умолчанию и проверяет диапазоны (см. «Граничные случаи»).

### 2. Выбор региона (region.rs)

Ключевая таблица для O(1) поиска на NoteOn (RFC §4: «индексация по ноте и
линейный просмотр ≤ MAX_PER_KEY»):

```rust
pub const MAX_PER_KEY: usize = 64;
pub const MIDI_NOTES: usize = 128;

/// Таблица `[Vec<u16>; 128]`: для каждой ноты — индексы регионов, покрывающих
/// её по ключу. Строится при загрузке; на NoteOn просматриваются только
/// регионы нужной ноты, выбирается первый, у которого `vel_lo <= velocity <=
/// vel_hi` (приоритет — порядок в .sfz, т.е. более специфичный регион раньше).
pub struct RegionTable {
    pub by_key: [Vec<u16>; MIDI_NOTES],
}
```

Если регионов на одну ноту больше `MAX_PER_KEY`, лишние отбрасываются при
загрузке (счётчик `truncated_regions` для диагностики). Выбор по velocity —
линейный просмотр, без сортировок и `HashMap`.

### 3. Голос (voice.rs)

```rust
/// Длительность быстрого fade-out при вытеснении/off_by, в отсчётах ≈ 5 мс.
pub const STEAL_FADE_SAMPLES: u32 = 240; // 5 мс при 48 кГц

/// Sentinel «голос свободен».
pub const NO_REGION: u16 = u16::MAX;

/// Один голос сэмплера. Позиция чтения — `f64`; огибающая и позиция считаются
/// от абсолютного сэмпла (как `Synth::Voice`), поэтому выход не зависит от
/// размера кванта.
pub struct SamplerVoice {
    pub region: u16,          // u16::MAX = idle
    pub note: u8,
    pub velocity: u8,
    pub pos: f64,             // позиция чтения в сэмпле (кадры)
    pub ratio: f64,           // шаг позиции на выходной отсчёт
    pub peak: f32,            // пик до огибающей (velocity · gain · pan)
    pub pan_l: f32,           // веса панорамы (стерео-выход)
    pub pan_r: f32,
    pub started_at: u64,
    pub released_at: Option<u64>,
    pub release_level: f32,
    pub fade_start: u64,      // начало быстрого fade
    pub fade_from: f32,       // уровень на момент начала fade
    pub fading: bool,
    pub loop_enabled: bool,   // loop_continuous / loop_sustain
    pub loop_start: f64,
    pub loop_end: f64,
    pub end_frame: f64,       // последний воспроизводимый кадр (exclusive)
    pub envelope: AmpEnvelope,
    pub last_gain: f32,       // уровень на последнем рендеренном отсчёте
}
```

Голос имеет `const fn idle()`, `is_idle()`, `gain()` (последний уровень, для
порядка вытеснения) по образцу `Synth::Voice`. Огибающая ADSR с отрезками
attack → hold → decay → sustain → release (release стартует от текущего
уровня, как в `synth.rs`), все фазы — линейные. При быстром fade (вытеснение,
off_by fast) уровень линейно падает до нуля за `STEAL_FADE_SAMPLES` отсчётов
поверх обычной огибающей.

### 4. Движок (engine.rs)

```rust
/// Максимальное число голосов (верхняя граница `SamplerEngine::new`).
pub const MAX_SAMPLER_VOICES: usize = 128;
/// Пул по умолчанию (RFC §3.1).
pub const DEFAULT_VOICES: usize = 64;

/// Движок сэмплера: пул голосов, выбор региона, вытеснение, off_by.
pub struct SamplerEngine {
    voices: Vec<SamplerVoice>,   // ёмкость задаётся в new, фиксирована
    table: RegionTable,
    regions: Vec<SampleRegion>,  // иммутабельные данные банка
    samples: Vec<Arc<[i16]>>,    // PCM i16, предзагружен
    sample_rate: u32,
    started: AtomicU32,           // счётчик стартов (диагностика)
    stolen: AtomicU32,            // счётчик вытеснений
    dropped_starts: AtomicU32,    // старты, отброшенные за квант
}
```

- `new(sample_rate, regions, samples, num_voices)` — возвращает `Result`, при
  нулевом `sample_rate`, `num_voices == 0` или `num_voices > MAX_SAMPLER_VOICES`
  возвращает `SamplerError`. Весь пул голосов (`Vec<SamplerVoice>`),
  `RegionTable` и копия `regions`/`samples` (как `Arc`) выделяются здесь;
  дальше на горячем пути аллокаций нет.
- Выбор голоса на NoteOn (RFC §3.1, порядок строго): (1) свободный голос;
  (2) самый тихий голос в release (не fading); (3) самый старый удерживаемый
  (при равенстве — с минимальной velocity); (4) голос не стартует
  (`dropped_starts += 1`), никогда не блокируем.
- Вытеснение жертвы — быстрый fade-out за `STEAL_FADE_SAMPLES`; новый голос
  стартует с точки события (сэмпл-точная нарезка квантом, как `Synth::render`).
- `off_by`: NoteOn региона обрезает все голоса группы `off_by` (fast = 5 мс
  fade, normal = `ampeg_release` региона-жертвы).
- Счётчики `started`, `stolen`, `dropped_starts` — `AtomicU32`, читает UI.

Два независимых сброса (не путать):

- `reset()` — глушит все голоса **и** fade-хвосты (`SamplerVoice::idle()`),
  статистику не трогает. Вызывается после seek назад, чтобы ноты, начатые до
  новой позиции, не дозвучивали.
- `reset_voice_counters()` — сбрасывает только три счётчика (`started`,
  `stolen`, `dropped_starts`) в ноль, голоса не трогает. Позволяет обнулить
  lifetime-статистику без остановки звучания.

Разделение нужно, потому что seek и сброс диагностики — разные операции и не
должны быть связаны друг с другом.

### 5. Офлайн-рендер (engine.rs)

```rust
pub fn render(
    &mut self,
    schedule: &Schedule,
    cursor: &mut ScheduleCursor,
    position: u64,
    frames: usize,
    out_l: &mut [f32],
    out_r: &mut [f32],
)
```

Тот же движок используется офлайн-рендером (RFC §5). `out_l`/`out_r` —
предоставляемые вызывающим буферы; пишутся только первые `frames` отсчётов,
остальное не трогается. Рендер стерео — каждый голос масштабируется весами
панорамы `pan_l`/`pan_r` и суммируется прямо в выходные буферы (отдельного
scratch-буфера нет). API `render(schedule, ...)` в буфер вызывающего —
как `Synth::render`, но стерео.

### 6. Интерполяция (voice.rs)

Линейная по умолчанию (RFC §3.2): `a + (b − a) · frac` между соседними
кадрами `i16` → `f32` (перевод в `f32` на лету). Hermite — PR 4, в v1 не
включается. Шаг `ratio` — свободная функция `playback_ratio(note, root,
tune_cents, sample_rate)` в `engine.rs` по формуле RFC §3.2:

```text
ratio = 2^((note − root)/12 + tune/1200) · file_rate / out_rate
```

(здесь `tune` в центах — поправка `tune/1200` добавляется к показателю; в v1
`file_rate == out_rate`, т.к. сэмплы предварительно не ресемплируются — ресемпл
по желанию в PR 3, на лету по `ratio`.)

## Граничные случаи

| Случай | Ожидаемое поведение |
|---|---|
| `num_voices == 0` или `> MAX_SAMPLER_VOICES` | `SamplerError::InvalidVoices` |
| `sample_rate == 0` | `SamplerError::InvalidSampleRate` |
| регион без сэмпла / `sample_index` за пределами `samples` | отбрасывается при загрузке (не попадает в таблицу) |
| `lokey > hikey` или `lovel > hivel` | регион отбрасывается при загрузке |
| `loop_end <= loop_start` при loop-режиме | регион играется как `NoLoop` (без цикла), без паники |
| `end == 0` | конец — длина сэмпла; `offset >= длина` → регион молчит |
| NoteOn вне диапазона velocity всех регионов ноты | голос не стартует, счётчик `dropped_starts` |
| переполнение пула (64 голоса заняты) | вытеснение по RFC §3.1, счётчик `stolen` |
| NoteOff для несуществующего голоса | no-op |
| регион `off_by` на голос той же ноты | жертва уходит в быстрый fade, новый голос стартует |
| velocity 0 | парсер превращает в NoteOff; движок игнорирует NoteOn с velocity 0 |

## Критерии приёмки

- [ ] `cargo fmt --all --check` зелёный
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` зелёный
- [ ] `cargo test --workspace --all-targets` зелёный
- [ ] Рендер без аллокаций на горячем пути: тест с перехватом аллокаций
      (counting-аллокатор). Тест обязан прогнать **весь жизненный цикл**
      голоса внутри окна измерения, а не один моно-нот-он: переполнение пула
      >64 голосов (вытеснение + fade-хвост), `off_by`-choke, стерео-сэмпл,
      loop, note-off и освобождение голоса. В конце — `voices_started() > 0`
      и ненулевой выходной сигнал, иначе тест ничего не доказывает. Ссылка на
      несуществующий пункт RFC (напр. «§3.5») недопустима.
- [ ] Тесты: точность частоты/питча, маппинг velocity, вытеснение голосов,
      choke-группы, огибающая, детерминизм офлайн-рендера (бит-в-бит при
      разных размерах кванта), отсутствие аллокаций.

## Нельзя

- Менять `crates/audio-core/src/sampler/sfz.rs` (парсер из SPEC-062).
- Реализовывать загрузку файлов, декодирование FLAC, управление банками,
  трейт `Instrument`, `SamplerNode`, hot_path-проводку, GUI, сеть, изменение
  формата проекта.
- Трогать `vendor/winit`, `SOUND_PRO_PATCH.md`, `x11_cursor.rs`, `tests/drop.rs`,
  goldens, `docs/PROJECT_STATE.md`, `docs/ROADMAP.md`, существующий
  realtime-движок и существующий piano-рендерер.
- Использовать `unsafe`, аллокации/блокировки/I/O на горячем пути.
- Добавлять тяжёлые зависимости (criterion) — только дешёвый timing-тест
  или документированная заметка о бенче.
